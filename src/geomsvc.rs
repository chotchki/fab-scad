//! The geometry SERVICE (C.2): `geomsg::Request` in, `geomsg::Response` out — every kernel op
//! the web app needs behind one seam. Runs in the fab-geom worker on wasm, on a task pool
//! natively. Solids never cross the boundary (the !Send contract): bytes in, bytes out.

use anyhow::{Context, Result};

use crate::geomsg::*;
use crate::kernel::Solid;
use crate::manifest::{Connector, Cut, Slicing};
use crate::num::Num;
use crate::{auto, auto_slice, slicing, stl, threemf_in};
use fab_lang::{Affine, Dims, Tri, Vec3};

/// The service: never panics outward, never errors the transport — failures are a Response.
pub fn handle(req: Request) -> Response {
    let run = || -> Result<Response> {
        match req {
            Request::Analyze { name, bytes, bed } => analyze(&name, &bytes, Dims::from_array(bed)),
            Request::Slice {
                objects,
                cuts,
                connectors,
                with_connectors,
            } => slice(&objects, &cuts, &connectors, with_connectors),
            Request::Export {
                objects,
                cuts,
                connectors,
                bed,
                gap,
            } => export(&objects, &cuts, &connectors, Dims::from_array(bed), gap),
            Request::Section { objects, axis, at } => Ok(Response::Sectioned {
                loops: rotated_union(&objects)?.cross_section(axis, at),
            }),
        }
    };
    run().unwrap_or_else(|e| Response::Failed {
        error: format!("{e:#}"),
    })
}

fn analyze(name: &str, bytes: &[u8], bed: Dims) -> Result<Response> {
    // Per object: (display-fallback bytes, maybe a Solid, color, tri count).
    let mut solids: Vec<Solid> = Vec::new();
    let mut raw: Vec<GeomObject> = Vec::new();
    let mut colors: Vec<Option<[f32; 4]>> = Vec::new();
    let mut tris = 0usize;
    let mut all_solid = true;
    if name.to_ascii_lowercase().ends_with(".3mf") {
        for o in threemf_in::parse_3mf(bytes)? {
            tris += o.tris.len();
            // The 3mf reader speaks raw [f64;3]/[u32;3]; lift to the kernel's Vec3/Tri at the boundary.
            let verts: Vec<Vec3> = o.verts.iter().map(|&v| Vec3::from_array(v)).collect();
            let faces: Vec<Tri> = o.tris.iter().map(|&t| Tri(t)).collect();
            match Solid::from_indexed(&verts, &faces) {
                Ok(s) => solids.push(s),
                Err(_) => all_solid = false,
            }
            raw.push(GeomObject {
                stl: stl::binary_from_indexed(&o.verts, &o.tris),
                color: o.color,
            });
            colors.push(o.color);
        }
    } else {
        tris += stl::load_stl_bytes(bytes)?.positions.len() / 3;
        match Solid::from_stl_bytes(bytes) {
            Ok(s) => solids.push(s),
            Err(_) => all_solid = false,
        }
        raw.push(GeomObject {
            stl: bytes.to_vec(),
            color: None,
        });
        colors.push(None);
    }
    if !all_solid || solids.is_empty() {
        return Ok(Response::Analyzed {
            objects: raw,
            plan: None,
            tris,
        });
    }

    let union = match solids.len() {
        1 => solids[0].transform(&Affine::IDENTITY),
        _ => Solid::batch_union(&solids),
    };
    let fit = auto_slice::best_fit_rotation(&union, bed);
    let planned = auto::plan(&union.transform(&fit.rot), fit.min, fit.max, bed)?;
    let objects = solids
        .iter()
        .zip(&colors)
        .map(|(s, c)| GeomObject {
            stl: s.transform(&fit.rot).to_stl_bytes(),
            color: *c,
        })
        .collect();
    Ok(Response::Analyzed {
        objects,
        plan: Some(PlanOut {
            rot: fit.rot.to_column_major(), // the wire carries the column-major matrix, as before
            min: fit.min.to_array(),
            max: fit.max.to_array(),
            cuts: planned.cuts,
            connectors: planned.connectors.iter().map(WireConn::from).collect(),
        }),
        tris,
    })
}

fn spec(cuts: &[(char, f64)], connectors: Vec<Connector>) -> Slicing {
    Slicing {
        printer: None,
        cut: cuts
            .iter()
            .map(|&(ax, at)| Cut {
                axis: ax.to_string(),
                at: Num::Float(at),
            })
            .collect(),
        connector: connectors,
        orient: vec![],
    }
}

fn slice(
    objects: &[GeomObject],
    cuts: &[(char, f64)],
    connectors: &[WireConn],
    with_connectors: bool,
) -> Result<Response> {
    let conns = if with_connectors {
        connectors.iter().map(Connector::from).collect()
    } else {
        vec![]
    };
    let s = spec(cuts, conns);
    let mut pieces = Vec::new();
    for o in objects {
        let solid = Solid::from_stl_bytes(&o.stl).context("piece source")?;
        for (idx, cell) in slicing::slice_solid(&s, &solid)? {
            pieces.push(Piece {
                idx,
                stl: cell.to_stl_bytes(),
                color: o.color,
            });
        }
    }
    Ok(Response::Sliced { pieces })
}

fn export(
    objects: &[GeomObject],
    cuts: &[(char, f64)],
    connectors: &[WireConn],
    bed: Dims,
    gap: f64,
) -> Result<Response> {
    let base = rotated_union(objects)?;
    let mut buf = std::io::Cursor::new(Vec::new());
    let sum = auto::make_planned(
        base,
        cuts,
        connectors.iter().map(Connector::from).collect(),
        bed,
        &mut buf,
        gap,
    )?;
    Ok(Response::Exported {
        threemf: buf.into_inner(),
        pieces: sum.pieces,
        plates: sum.plates,
    })
}

fn rotated_union(objects: &[GeomObject]) -> Result<Solid> {
    let solids: Vec<Solid> = objects
        .iter()
        .map(|o| Solid::from_stl_bytes(&o.stl))
        .collect::<Result<_>>()?;
    Ok(match solids.len() {
        0 => Solid::batch_union(&[]),
        1 => solids.into_iter().next().expect("len checked"),
        _ => Solid::batch_union(&solids),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_cube() -> (Vec<GeomObject>, PlanOut) {
        let cube = Solid::cube(30.0, 30.0, 60.0, false).to_stl_bytes();
        match handle(Request::Analyze {
            name: "t.stl".into(),
            bytes: cube,
            bed: [40.0; 3],
        }) {
            Response::Analyzed {
                objects,
                plan: Some(p),
                ..
            } => (objects, p),
            other => panic!("expected planned analyze, got {}", label(&other)),
        }
    }

    fn label(r: &Response) -> &'static str {
        match r {
            Response::Analyzed { .. } => "Analyzed",
            Response::Sliced { .. } => "Sliced",
            Response::Exported { .. } => "Exported",
            Response::Sectioned { .. } => "Sectioned",
            Response::Failed { .. } => "Failed",
        }
    }

    #[test]
    fn analyze_slice_export_round_trip_through_the_wire() {
        let (objects, plan) = analyze_cube();
        assert_eq!(plan.cuts.len(), 1, "60mm on a 40 bed → one cut");

        // Through the CODEC, like the worker sees it.
        let req = decode_request(&encode_request(&Request::Slice {
            objects: objects.clone(),
            cuts: plan.cuts.clone(),
            connectors: plan.connectors.clone(),
            with_connectors: true,
        }))
        .unwrap();
        let Response::Sliced { pieces } = handle(req) else {
            panic!("slice failed")
        };
        assert_eq!(pieces.len(), 2);

        let Response::Exported {
            threemf, pieces, ..
        } = handle(Request::Export {
            objects,
            cuts: plan.cuts,
            connectors: plan.connectors,
            bed: [40.0; 3],
            gap: 5.0,
        })
        else {
            panic!("export failed")
        };
        assert_eq!(pieces, 2);
        assert!(threemf.len() > 1000);
    }

    #[test]
    fn analyze_reports_view_only_for_soup() {
        // Open-shell soup: one triangle. Displays, never welds.
        let soup = stl::binary_from_indexed(
            &[[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]],
            &[[0, 1, 2]],
        );
        match handle(Request::Analyze {
            name: "t.stl".into(),
            bytes: soup,
            bed: [40.0; 3],
        }) {
            Response::Analyzed {
                plan: None, tris, ..
            } => assert_eq!(tris, 1),
            other => panic!("expected view-only, got {}", label(&other)),
        }
    }

    #[test]
    fn section_returns_the_editor_profile() {
        let (objects, plan) = analyze_cube();
        let (_, at) = plan.cuts[0];
        let Response::Sectioned { loops } = handle(Request::Section {
            objects,
            axis: 2,
            at,
        }) else {
            panic!("section failed")
        };
        assert_eq!(loops.len(), 1, "solid square profile: one outline");
    }

    #[test]
    fn failures_travel_as_responses_not_panics() {
        let r = handle(Request::Analyze {
            name: "t.stl".into(),
            bytes: vec![1, 2, 3],
            bed: [40.0; 3],
        });
        assert!(matches!(r, Response::Failed { .. }));
    }
}
