//! Shared vocabulary: the resources, components, messages and plain types used across concerns. Everything else depends downward on this, never sideways.

use crate::*;

pub(crate) const SPREAD: f64 = 50.0;

/// Scene inputs shared by both modes.
#[derive(Resource, Clone)]
pub(crate) struct SceneCfg {
    pub(crate) source: Option<PathBuf>, // .scad source (sliceable, preferred)
    pub(crate) stl: Option<PathBuf>,    // .stl to display directly (when there's no source)
    pub(crate) bed: [f32; 2],   // usable area — pieces pack within this (extruder reach)
    pub(crate) plate: [f32; 2], // real plate size for the Bambu export grid/printable_area (≥ bed)
    pub(crate) root: Option<PathBuf>, // workspace root, for OPENSCADPATH
    pub(crate) tmp: PathBuf,          // scratch dir for rendered/sliced STLs
    pub(crate) reslice_on_start: bool, // screenshot --reslice: display the sliced result
    pub(crate) cut_pct: f32,          // screenshot --cut <0..100>: where along X to cut
}

/// Marks the displayed model entity, so re-slice can swap it out.
#[derive(Component)]
pub(crate) struct Model;

/// Tags a displayed model entity with its part index (into [`Parts`]). The mesh-swap systems
/// (explode/collapse/edit-revert/reslice) filter on it so an edit to one part swaps only THAT
/// part's mesh, never the others (T.2b). One part → always `PartId(0)`.
#[derive(Component, Clone, Copy)]
pub(crate) struct PartId(pub(crate) usize);

/// Marks the printer-bed slab, so `seat_bed` can drop it to the model's Z-floor (the model's native
/// coords may put its bottom below z=0; move the bed to meet it rather than shift the model — which
/// would desync the cut positions from the source the slicer re-renders).
#[derive(Component)]
pub(crate) struct Bed;

/// One plate slab in the print-orientation preview — a bed-sized floor tile at its cell in the
/// near-square plate grid (`pack::plate_origin`). `sync_orientation` spawns one per packed plate so
/// the preview shows the SAME grid the `.3mf` export writes; despawned with the pieces when the
/// preview relays out or closes. The single startup [`Bed`] hides while these are up.
#[derive(Component)]
pub(crate) struct PrintPlate;

/// Button → "re-slice the source and swap the mesh".
#[derive(Message)]
pub(crate) struct ReSlice;

/// Button / `autoplace` verb → "fill the open cut's cross-section with auto-sized onions".
#[derive(Message)]
pub(crate) struct AutoPlace;

/// A file-list row click / the `open` script verb / the picker landing → "make file <i> the active
/// source": wipe the old model's state and render the new one (`apply_switch_file`).
#[derive(Message, Clone, Copy)]
pub(crate) struct SwitchFile(pub(crate) usize);

/// The browsable source list (5.3.2): every `.scad` the picker turned up, plus which one is active.
/// `SceneCfg.source` stays the single source of truth for "what's loaded" — this just adds the list
/// the panel shows and the switch machinery indexes. Empty until the first Open.
#[derive(Resource, Default)]
pub(crate) struct FileList {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) active: Option<usize>,
}

/// The in-flight native folder pick (5.3.1), off the main thread like a render job. `Some(path)` on
/// pick, `None` if the user cancelled; `poll_open_dialog` drains it into `FileList`.
#[derive(Resource, Default)]
pub(crate) struct OpenDialog(pub(crate) Option<Task<Option<PathBuf>>>);

/// The Model-tab code editor's live buffer (U.3.2): the active file's text, edited in place. The
/// buffer — NOT the file on disk — is the render source; `preview_edited_buffer` writes it to a
/// hidden temp beside the real file (so relative includes resolve) and re-renders after a debounce.
/// `dirty` gates the explicit Save; `edited_at` (Bevy elapsed secs at the last keystroke) drives it.
#[derive(Resource, Default)]
pub(crate) struct EditorBuf {
    pub(crate) text: String,
    pub(crate) path: PathBuf,
    pub(crate) dirty: bool,
    pub(crate) edited_at: Option<f64>,
}

/// Load `path`'s text into the editor buffer, EXTRACTING its `fab:config` block (W.3.8): the block is
/// stripped from the buffer (the editor shows the clean model — chotchki's call) and its parsed config
/// RETURNED for the caller to stash into [`PendingConfig`] + apply once the parts are built. Clean (not
/// dirty, no pending edit). Used on the launch seed + every file switch.
pub(crate) fn read_into_editor(
    editor: &mut EditorBuf,
    path: &Path,
) -> Option<Vec<fab_scad::manifest::PartSlicing>> {
    editor.path = path.to_path_buf();
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let cfg = config::read_config_block(&raw);
    editor.text = config::strip_config_block(&raw);
    editor.dirty = false;
    editor.edited_at = None;
    cfg
}

/// Per-part slicing config parsed from a freshly-loaded source's `fab:config` block (W.3.8), waiting for
/// `poll_job` to apply it once the parts are built (the block loads BEFORE the render that makes the
/// parts). `read_into_editor` fills it; `poll_job` takes it. `None` = no block → parts auto-derive.
#[derive(Resource, Default)]
pub(crate) struct PendingConfig(pub(crate) Option<Vec<fab_scad::manifest::PartSlicing>>);

/// A finished render/slice job's payload (T.2b). A whole render mints ALL top-level parts' base solids
/// in the geometry service (W.3.3); a reslice reads ONE held base and hands back the sliced STL bytes.
pub(crate) enum JobResult {
    /// Every rendered top-level part (minted handle + display STL + bbox + provenance). `fresh` = a new
    /// source (replace the parts list); else a reload of the SAME source (refresh geometry in place,
    /// keeping each part's cuts/connectors) — the old handles are freed either way (`poll_job`).
    Rendered {
        fresh: bool,
        parts: Vec<RenderedPart>,
    },
    /// One part's sliced STL BYTES — the part index it belongs to (its `Model` entity carries `PartId`).
    Resliced { part: usize, stl: Vec<u8> },
}

/// One rendered top-level part off the wire: its minted base handle, display STL bytes, world-space
/// bbox, and provenance name. `poll_job` builds a `Part` + `Model` entity from each (mesh from bytes,
/// bounds from min/max) — no disk round-trip.
pub(crate) struct RenderedPart {
    pub(crate) base: SolidId,
    pub(crate) stl: Vec<u8>,
    pub(crate) min: [f64; 3],
    pub(crate) max: [f64; 3],
    pub(crate) name: Option<String>,
}

/// The in-flight render/slice (off the main thread). Yields a [`JobResult`] on success, else an error.
#[derive(Resource, Default)]
pub(crate) struct Job(pub(crate) Option<Task<Result<JobResult, String>>>);

/// One-line status shown in the panel (e.g. "slicing", "ready").
#[derive(Resource)]
pub(crate) struct Status(pub(crate) String);

/// The axis a cut plane is normal to (which way it slices).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum Axis {
    #[default]
    X,
    Y,
    Z,
}

impl Axis {
    pub(crate) fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
    pub(crate) fn unit(self) -> Vec3 {
        match self {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Axis::X => "X",
            Axis::Y => "Y",
            Axis::Z => "Z",
        }
    }
    /// The slicer's axis letter.
    pub(crate) fn scad(self) -> char {
        match self {
            Axis::X => 'x',
            Axis::Y => 'y',
            Axis::Z => 'z',
        }
    }
}

/// One planar cut: which axis it's normal to, its position along that axis, and whether it's in
/// the slice.
#[derive(Clone)]
pub(crate) struct CutDef {
    pub(crate) axis: Axis,
    pub(crate) at: f32,
    pub(crate) enabled: bool,
}

/// The ordered cut stack + which cut the drag edits. A slice is a pure function of
/// (source, enabled cuts) — the node a DAG cache will key on.
#[derive(Default)]
pub(crate) struct Cuts {
    pub(crate) list: Vec<CutDef>,
    pub(crate) active: usize,
}

impl Cuts {
    /// Enabled cuts as `(axis letter, position)`, the input to `fab::reslice`.
    pub(crate) fn enabled_cuts(&self) -> Vec<(char, f64)> {
        self.list
            .iter()
            .filter(|c| c.enabled)
            .map(|c| (c.axis.scad(), c.at as f64))
            .collect()
    }

    /// Stack indices of the enabled cuts, in order — a connector's stack-index maps to its
    /// position here to reference the right cut in the sliced spec.
    pub(crate) fn enabled_indices(&self) -> Vec<usize> {
        self.list
            .iter()
            .enumerate()
            .filter(|(_, c)| c.enabled)
            .map(|(i, _)| i)
            .collect()
    }

    pub(crate) fn active_axis(&self) -> Axis {
        self.list
            .get(self.active)
            .map(|c| c.axis)
            .unwrap_or(Axis::X)
    }
}

/// Machine-screw size for bolt connectors; `label` is the manifest / BOSL2 string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Screw {
    M3,
    M4,
    M5,
}

impl Screw {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Screw::M3 => "M3",
            Screw::M4 => "M4",
            Screw::M5 => "M5",
        }
    }
    /// Approx socket-head / counterbore radius (mm) — the bolt's footprint in the editor profile.
    pub(crate) fn head_r(self) -> f32 {
        match self {
            Screw::M3 => 2.75,
            Screw::M4 => 3.5,
            Screw::M5 => 4.25,
        }
    }
}

/// A placed connector: which cut (stack index) it sits on, its position in that cut plane's two
/// non-axis dims, its onion diameter (auto-sized at placement; ignored for a bolt), and its `kind`
/// + bolt `screw` — set from the active type when placed, so onion and bolt can mix on one cut.
#[derive(Clone, Copy)]
pub(crate) struct PlacedConn {
    pub(crate) cut: usize,
    pub(crate) pos: [f32; 2],
    pub(crate) size: f32,
    pub(crate) kind: fab::ConnKind,
    pub(crate) screw: Screw,
}

/// The placed connectors (manual face-pick). Like the cut stack, a pure input to the slice.
#[derive(Default)]
pub(crate) struct Conns {
    pub(crate) list: Vec<PlacedConn>,
}

/// The kind + screw NEW placements take (manual click + Auto-place). Existing connectors keep their
/// own — you can mix onion and bolt on a cut. Set by the connector editor's type selector.
#[derive(Resource, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveConn {
    pub(crate) kind: fab::ConnKind,
    pub(crate) screw: Screw,
}

impl Default for ActiveConn {
    fn default() -> Self {
        Self {
            kind: fab::ConnKind::Onion,
            screw: Screw::M3,
        }
    }
}

/// Which cut's 2D connector editor is open (None = normal 3D view). When set, the model hides and
/// the cut's cross-section profile is shown face-on for precise picking.
#[derive(Resource, Default)]
pub(crate) struct EditCut(pub(crate) Option<usize>);

/// The open cut's cross-section: profile loops in connector-pos coords (the cut's two non-axis
/// dims). `None` until computed / when no editor is open.
#[derive(Resource, Default)]
pub(crate) struct XSection(pub(crate) Option<Vec<Vec<[f32; 2]>>>);

/// Per-piece print orientations, keyed by slab multi-index — the build-up direction (model space)
/// each piece prints in. The preview seeds `map` with the auto-pick (`auto_orient::best_up`);
/// clicking a piece's face sets a MANUAL override (recorded in `manual` so a re-render keeps it).
/// Threaded into `reslice` so the slice gates its onions on how each piece actually prints. Empty =
/// every piece defaults to +Z (the pre-orientation behaviour).
/// A printable piece's identity WITHIN one part: its slab multi-index + its connected-COMPONENT
/// index within that slab (0 when the slab is a single solid; a presliced blob splits into comps
/// 0..N — T.2a). Every per-piece orientation in a part's [`Orient`] map keys off this so each
/// component orients on its own.
pub(crate) type PieceKey = ([usize; 3], usize);

/// A laid-out print piece's identity ACROSS parts (T.2b co-pack): its part index + its per-part
/// [`PieceKey`]. The `(slab, comp)` key collides across parts (every part has a slab `[0,0,0]`), so
/// the part prefix routes each laid-out piece back to `parts.0[part].orient` — the authoritative
/// per-part store stays keyed by [`PieceKey`], the part index is a routing prefix, not a map dimension.
pub(crate) type PrintId = (usize, [usize; 3], usize);

#[derive(Default)]
pub(crate) struct Orient {
    pub(crate) map: HashMap<PieceKey, [f32; 3]>,
    pub(crate) manual: HashSet<PieceKey>,
}

impl Orient {
    /// Record a user-chosen build-up for `key` (model space, normalised by the caller).
    pub(crate) fn set_manual(&mut self, key: PieceKey, up: [f32; 3]) {
        self.map.insert(key, up);
        self.manual.insert(key);
    }
    /// This piece's build-up, falling back to `auto` (the auto-pick) when unset.
    pub(crate) fn up_or(&self, key: PieceKey, auto: [f32; 3]) -> [f32; 3] {
        self.map.get(&key).copied().unwrap_or(auto)
    }
    /// Drop `key`'s override so the piece falls back to its auto-pick (the per-piece "reset to auto",
    /// U.3.4): out of `manual` re-flags it as auto in the list, out of `map` makes [`up_or`](Self::up_or)
    /// return the caller's auto fallback. A re-layout (`parts.set_changed()`) then re-seats it.
    pub(crate) fn reset(&mut self, key: PieceKey) {
        self.map.remove(&key);
        self.manual.remove(&key);
    }
}

/// One independent top-level part of the model (T.2b): its own cut stack, connectors, per-piece
/// orientations, model bbox, and auto-plan-done flag. The whole per-model state that USED to be five
/// global resources now lives here, one bundle per part. Increment A keeps exactly ONE Part so
/// behaviour is unchanged; Increment B builds N (one per `build_geo_parts` top-level item).
#[derive(Default)]
pub(crate) struct Part {
    pub(crate) cuts: Cuts,
    pub(crate) conns: Conns,
    pub(crate) orient: Orient,
    pub(crate) bounds: ModelBounds,
    /// Printable-piece (connected-component) count of this part's WHOLE geometry, stamped by
    /// `poll_auto_plan` (0 = not computed yet). A PRESLICED part is ONE part made of many disjoint
    /// pieces; the accordion header maxes this against the cut estimate so a 6-piece presliced blob
    /// reads "6 pcs", not "1 pc" (T.2a).
    pub(crate) pieces: usize,
    pub(crate) auto_planned: AutoPlanned,
    // Display state — was three global resources (WholeMesh/SlicedMesh/DisplaySpread) when the scene
    // held ONE model; now per-part so N parts each explode/collapse on their own (T.2b).
    pub(crate) whole: Option<Handle<Mesh>>, // the uncut mesh — revert from exploded without re-rendering
    pub(crate) sliced: Option<Handle<Mesh>>, // the last sliced (exploded) mesh — re-show without re-slicing
    pub(crate) spread: f32,                  // 0 = uncut/editing, >0 = exploded (fan distance)
    /// This part's whole base solid, HELD in the geometry service (W.3.3) and addressed by handle —
    /// the reslice/edit/plan/section source. `None` until the render lands. The `!Send` Solid never
    /// leaves the service; this plain id is all the GUI keeps (replacing the old on-disk `base_stl`).
    pub(crate) base: Option<SolidId>,
    pub(crate) sliced_hash: Option<u64>, // `slice_hash` of the inputs last resliced — per-part so editing A never reslices B
    pub(crate) name: Option<String>, // the top-level module/function that produced this part (T.2b provenance); None = anonymous
}

/// The model's parts. INVARIANT: always non-empty — `[ActivePart]` indexes the one the panel edits.
#[derive(Resource, Default)]
pub(crate) struct Parts(pub(crate) Vec<Part>);

/// Which part the panel + slice systems currently act on (index into [`Parts`]). Always valid.
#[derive(Resource, Default)]
pub(crate) struct ActivePart(pub(crate) usize);

/// Per-node pipeline feedback (U.3.7): which workflow STAGES are stale (their output is behind their
/// input), which are actively COMPUTING right now, and an accurate label for what's running. The tab
/// bar shows a spinner on a `loading` stage and an amber dot on a `dirty`-but-idle one; the status bar
/// pulses `activity` while busy (so it never lies "ready" mid-render). The DAG is Model(source) →
/// Parts(geometry) → Orientation(sliced+laid pieces) → Export(packed plates); a stage is dirty when an
/// upstream input changed since it last computed. `geo_of`/`layout_of` are the "last-computed-for"
/// hashes the completion systems (`poll_job`, `poll_print_job`) stamp; `sync_pipeline` derives the rest.
#[derive(Resource, Default)]
pub(crate) struct Pipeline {
    /// Source hash of the geometry currently displayed — stamped by `poll_job` on a render result.
    /// `None` until the first render (a not-yet-computed stage reads CLEAN, not stale).
    pub(crate) geo_of: Option<u64>,
    /// All-parts slice-config hash the current print layout was built from — stamped by `poll_print_job`.
    pub(crate) layout_of: Option<u64>,
    /// Per [`Tab`] (Model, Parts, Orientation, Export): this stage's output is behind its input.
    pub(crate) dirty: [bool; 4],
    /// Per [`Tab`]: this stage's geometry/layout is being COMPUTED right now (a job for it is in
    /// flight). Unlike `dirty` this fires on the FIRST compute too (a fresh model has no stale stage
    /// but is very much loading) — the spinner badge derives from this, not `dirty`.
    pub(crate) loading: [bool; 4],
    /// Any background job (render / reslice / plan / print) in flight → the status-bar pulse.
    pub(crate) busy: bool,
    /// An accurate one-line label for what's running (`"rebuilding geometry…"`, `"auto-planning part
    /// 2…"`, `"orienting pieces…"`), or `None` when idle. The status bar shows THIS while `busy`
    /// instead of the last imperative `Status` string, so the pulse can never read a stale "ready".
    pub(crate) activity: Option<String>,
}

/// Which entry-point environment the GUI runs in (U.3.6). `Desktop` = the full folder picker + the
/// ＋ file tab (open anything); `Web` = a single presupplied file, no picker (wasm has no folder
/// access), landing straight on the editor. Defaults from the build target — a test overrides it to
/// exercise the web path on desktop (the web host in ../hotchkiss-io serves the wasm build).
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Platform {
    Desktop,
    Web,
}

impl Default for Platform {
    fn default() -> Self {
        if cfg!(target_arch = "wasm32") {
            Platform::Web
        } else {
            Platform::Desktop
        }
    }
}

impl Platform {
    /// Whether to show the folder picker + ＋ (desktop only — web has one presupplied file).
    pub(crate) fn shows_picker(self) -> bool {
        matches!(self, Platform::Desktop)
    }
}

/// The co-pack preview summary (U.3.5): the plate/piece count + fill of packing the current print
/// pieces onto the bed, recomputed by `estimate_copack` when pieces or orientations change, read by the
/// Export tab. `bed` is cached alongside so the panel shows `fits WxH` without another system param.
/// `summary` is `None` until pieces exist and a pack lands (or when a piece can't fit the bed).
#[derive(Resource, Default)]
pub(crate) struct CoPack {
    pub(crate) summary: Option<fab_scad::bambu::ExportSummary>,
    pub(crate) bed: [f32; 2],
}

/// Whether the print-orientation preview is showing: the model + cut planes hide, and every piece
/// is laid out on the bed rotated to its print-up. A workflow MODE, like the connector editor.
#[derive(Resource, Default)]
pub(crate) struct PrintView(pub(crate) bool);

/// The source already auto-planned on open, so it fires ONCE per fresh too-big model — not every
/// frame, and not again after you clear the cuts by hand. Per-part ([`Part::auto_planned`]).
#[derive(Default)]
pub(crate) struct AutoPlanned(pub(crate) Option<PathBuf>);

/// The orbit camera (yaw, pitch, radius, target) as it was in NORMAL view, saved while there so a
/// mode that hijacks the camera (the 2D editor's face-on, the print preview's bed-frame) can hand
/// it back when you return. Without this, leaving a mode strands you at the mode's camera.
#[derive(Resource, Default)]
pub(crate) struct PrevCam(pub(crate) Option<(f32, f32, f32, Vec3)>);

/// Per-placed-connector onion feasibility (index-aligned with `Conns::list`): `true` = prints
/// support-free, `false` = downgrades to a bolt under the current orientations. Drives the marker
/// colour + the downgrade count. Recomputed when cuts / connectors / orientations change.
#[derive(Resource, Default)]
pub(crate) struct Feas(pub(crate) Vec<bool>);

/// The X/Y/Z component of `v`.
pub(crate) fn comp(v: Vec3, i: usize) -> f32 {
    match i {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

/// `v` with component `i` set to `val`.
pub(crate) fn with_comp(mut v: Vec3, i: usize, val: f32) -> Vec3 {
    match i {
        0 => v.x = val,
        1 => v.y = val,
        _ => v.z = val,
    }
    v
}

/// The whole model's AABB (min, max), set once on the first render — maps drag/positions. Per-part
/// ([`Part::bounds`]).
#[derive(Default)]
pub(crate) struct ModelBounds(pub(crate) Option<(Vec3, Vec3)>);

/// True while a cut plane is being dragged, so the camera orbit yields to it.
#[derive(Resource, Default)]
pub(crate) struct DraggingCut(pub(crate) bool);

/// True while the in-flight slice was kicked by `auto_reslice` (a background rebuild), so `poll_job`
/// refreshes the pieces WITHOUT jumping the view to exploded — vs an explicit slice, which shows them.
#[derive(Resource, Default)]
pub(crate) struct SliceInBackground(pub(crate) bool);

/// How long inputs must settle (no change) before a background reslice fires — coalesces a cut drag
/// or a burst of connector edits into ONE rebuild instead of one per frame.
pub(crate) const AUTOSLICE_DEBOUNCE: f32 = 0.35;

/// A panel button command that a heavy action system handles (U.1.2): the egui panel is
/// immediate-mode, so a click that needs params beyond the panel's own resources writes one of
/// these instead of mutating in place. The matching `*_action` system reads it.
#[derive(Message, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelCmd {
    AutoSlice,
    ToggleView,
    Publish,
    Export,
}

/// Panel → seam outputs, written by `panel_ui` each frame and read by the 3D systems: `over_ui`
/// yields the camera orbit when the pointer is on a panel; `width_px`/`top_px`/`bottom_px` inset the
/// 3D viewport inside the left panel + top tab bar + bottom status bar (U.3). Bundled into one
/// resource so `panel_ui` stays under Bevy's 16-param cap.
#[derive(Resource, Default)]
pub(crate) struct PanelSeam {
    pub(crate) over_ui: bool,
    pub(crate) width_px: f32,
    pub(crate) top_px: f32,
    pub(crate) bottom_px: f32,
}

/// Which top-level workflow tab is active (U.3). App-wide source of truth: the top tab bar sets it,
/// the left panel routes its content on it, and `sync_tab_modes` maps it onto the print/editor flags
/// the camera + visibility systems already react to. Model → Parts → Orientation → Export mirrors the
/// slice pipeline (source → cut → seat → pack); see docs/workflow-tabs-mockup.html.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum Tab {
    #[default]
    Model,
    Parts,
    Orientation,
    Export,
}

impl Tab {
    /// The four tabs in pipeline order with their bar labels.
    pub(crate) const ALL: [(Tab, &'static str); 4] = [
        (Tab::Model, "Model"),
        (Tab::Parts, "Parts"),
        (Tab::Orientation, "Orientation"),
        (Tab::Export, "Export"),
    ];
    /// Pipeline-order index — indexes [`Pipeline::dirty`] (Model 0 → Export 3).
    pub(crate) fn index(self) -> usize {
        match self {
            Tab::Model => 0,
            Tab::Parts => 1,
            Tab::Orientation => 2,
            Tab::Export => 3,
        }
    }
}

/// The auto-picked (eventually manual) orientations as `fab::Orient3` for `reslice`. Empty until the
/// print-orientation preview runs and seeds the map — then every slice honours them. The slice
/// codegen gates onions / teardrops per SLAB, so this projects the per-component map to slab-level
/// via component 0 (a multi-component slab is presliced ⇒ no connectors ⇒ this gates nothing).
pub(crate) fn orient_inputs(orient: &Orient) -> Vec<fab::Orient3> {
    orient
        .map
        .iter()
        .filter(|((_, comp), _)| *comp == 0)
        .map(|((piece, _), up)| fab::Orient3 {
            piece: *piece,
            up: [up[0] as f64, up[1] as f64, up[2] as f64],
        })
        .collect()
}

/// Map placed connectors to the sliced spec: a connector's stack-cut index → its position in the
/// enabled-cut list (which is what `fab::reslice` indexes). Connectors on a disabled cut drop out.
pub(crate) fn resolve_conns(cuts: &Cuts, conns: &Conns) -> Vec<fab::Conn> {
    let enabled = cuts.enabled_indices();
    conns
        .list
        .iter()
        .filter_map(|pc| {
            enabled
                .iter()
                .position(|&si| si == pc.cut)
                .map(|ei| fab::Conn {
                    cut: ei,
                    pos: [pc.pos[0] as f64, pc.pos[1] as f64],
                    size: pc.size as f64,
                    kind: pc.kind,
                    screw: pc.screw.label(),
                })
        })
        .collect()
}
