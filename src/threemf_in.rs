//! 3MF reader → per-object indexed meshes + display colors (A.9, TF). Our own quick-xml pull parser,
//! matching elements and attributes by namespace URI, never by prefix, so it reads what Bambu Studio
//! and MakerWorld write: the Production extension's cross-file components (`p:path`), `<metadata>`
//! anywhere in a model, DEFLATE entries. The `threemf` crate is the WRITER only (`Solid::write_3mf`):
//! its serde reader couldn't take metadata split around `<build>` and never saw `p:path`.
//!
//! What it reads is the `import/3mf` spec's to say. In short: only the ROOT model's build instantiates
//! (root = the start part `_rels/.rels` names, else `3D/3dmodel.model`); a component's object resolves
//! in the part its `p:path` names (default: its own part); transforms compose parent∘child and an absent
//! or identity component transform moves nothing, deliberately unlike OpenSCAD (see [`collect`]). Color
//! is the object-level basematerial `displaycolor`, resolved in the part that declares the object —
//! object-level pid is how slicers emit multi-color ASSEMBLIES; per-TRIANGLE overrides are ignored.
//! `unit` is ignored, as the oracle ignores it. `other` and `support` objects (Bambu's modifier,
//! negative-part and support-blocker volumes) contribute nothing, a mirrored placement is re-wound
//! outward, and a build that would expand past [`LIMITS`] fails before anything is allocated.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Cursor};

use anyhow::{Context, Result, anyhow, bail};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

/// One printable object out of the 3mf: indexed mesh (build/item transform applied) + color.
pub struct Object3mf {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    /// sRGBA 0..=1 from the object's basematerial, when it has one.
    pub color: Option<[f32; 4]>,
}

/// The OPC relationship type that names a 3MF package's root model (the "start part").
const START_PART_REL: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
/// Where the root model lives when `_rels/.rels` doesn't say (and where every producer puts it).
const DEFAULT_ROOT: &str = "3D/3dmodel.model";
/// Component nesting cap: a STACK guard (each level is a recursive call), not the cycle check, which
/// [`expanded_tris`] does exactly. No known producer nests past 2.
const MAX_DEPTH: usize = 64;

/// What a package may cost before anything is allocated for it (TF.11).
#[derive(Clone, Copy)]
struct Limits {
    /// Triangles the whole build may expand to, components instanced.
    triangles: u64,
    /// Decompressed bytes any one part may hold.
    part_bytes: u64,
}

const LIMITS: Limits = Limits {
    triangles: 50_000_000,
    part_bytes: 2 << 30,
};

/// Parse a `.3mf` (zip) into its built objects. Errors on a package with no meshes in its build and on
/// any reference it can't resolve, naming the part/object/index; tolerates missing materials (color =
/// None).
pub fn parse_3mf(bytes: &[u8]) -> Result<Vec<Object3mf>> {
    parse_3mf_with(bytes, LIMITS)
}

fn parse_3mf_with(bytes: &[u8], limits: Limits) -> Result<Vec<Object3mf>> {
    let mut pkg = Package::open(bytes, limits.part_bytes)?;
    let root = pkg.root()?;
    pkg.load_reachable(&root)?;
    let build = &pkg
        .parts
        .get(&root)
        .context("3mf root model failed to load")?
        .build;
    // Cost the build before building it: instancing is exponential in nesting depth, so a few KB of
    // components can describe billions of triangles.
    let mut memo = HashMap::new();
    let mut total = 0u64;
    for item in build {
        let at = item.path.as_ref().unwrap_or(&root);
        let n = expanded_tris(&pkg.parts, at, item.objectid, &mut memo, &mut Vec::new())?;
        total = total.saturating_add(n);
    }
    if total > limits.triangles {
        bail!(
            "3mf build expands to {total} triangles, past the {}-triangle limit",
            limits.triangles
        );
    }
    let mut out = Vec::new();
    for item in build {
        let at = item.path.as_ref().unwrap_or(&root);
        collect(&pkg.parts, at, item.objectid, item.transform, &mut out, 0)?;
    }
    if out.is_empty() {
        bail!("3mf has no printable meshes in its build");
    }
    Ok(out)
}

/// An OPC part name. OPC part names are case-insensitive and our callers spell them with or without
/// the leading `/`, so equality is on the normalized `key`; `raw` keeps the spelling for messages.
#[derive(Clone, Debug)]
struct PartName {
    key: String,
    raw: String,
}

impl PartName {
    fn new(spelled: &str) -> Self {
        let trimmed = spelled.trim_start_matches('/');
        Self {
            key: trimmed.to_ascii_lowercase(),
            raw: format!("/{trimmed}"),
        }
    }
}

impl PartialEq for PartName {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for PartName {}

impl Hash for PartName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl std::fmt::Display for PartName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

/// One parsed `.model` part. Object and material ids are per-part namespaces.
#[derive(Default)]
struct Part {
    objects: HashMap<u32, ObjectDef>,
    build: Vec<Instance>,
    /// Basematerial group id → displaycolors, in `<base>` order.
    materials: HashMap<u32, Vec<String>>,
}

#[derive(Default)]
struct ObjectDef {
    kind: ObjectKind,
    mesh: Option<MeshDef>,
    components: Vec<Instance>,
    pid: Option<u32>,
    pindex: Option<usize>,
}

/// The core `type` attribute. `other` and `support` objects aren't part of the printed model (3MF core),
/// and Bambu Studio writes its modifier, negative-part and support blocker/enforcer volumes as `other`.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ObjectKind {
    #[default]
    Model,
    SolidSupport,
    Support,
    Surface,
    Other,
}

impl ObjectKind {
    /// Unknown values read as `model`, the spec's default.
    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("other") => Self::Other,
            Some("support") => Self::Support,
            Some("solidsupport") => Self::SolidSupport,
            Some("surface") => Self::Surface,
            _ => Self::Model,
        }
    }

    fn printable(self) -> bool {
        !matches!(self, Self::Other | Self::Support)
    }
}

#[derive(Default)]
struct MeshDef {
    verts: Vec<[f64; 3]>,
    tris: Vec<[u32; 3]>,
}

/// A reference to an object: a build `<item>` or a `<component>`, which carry the same three things.
struct Instance {
    /// `p:path`: the part holding the object. `None` = the part this reference sits in.
    path: Option<PartName>,
    objectid: u32,
    transform: Option<[f64; 12]>,
}

/// A streaming, byte-capped reader over one zip entry.
type PartReader<'z, 'a> = BufReader<Capped<zip::read::ZipFile<'z, Cursor<&'a [u8]>>>>;

/// The open package: the zip, its entry names by part key, and the parts parsed so far.
struct Package<'a> {
    zip: zip::ZipArchive<Cursor<&'a [u8]>>,
    entries: HashMap<String, String>,
    parts: HashMap<PartName, Part>,
    part_bytes: u64,
}

impl<'a> Package<'a> {
    fn open(bytes: &'a [u8], part_bytes: u64) -> Result<Self> {
        let zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| anyhow!("3mf zip: {e}"))?;
        let entries = zip
            .file_names()
            .map(|n| (PartName::new(n).key, n.to_string()))
            .collect();
        Ok(Self {
            zip,
            entries,
            parts: HashMap::new(),
            part_bytes,
        })
    }

    fn has(&self, part: &PartName) -> bool {
        self.entries.contains_key(&part.key)
    }

    /// A streaming reader over one entry — parts are parsed straight out of the zip, never buffered
    /// whole, so a 100 MB sub-model costs its mesh, not its XML text too.
    fn entry(&mut self, part: &PartName) -> Result<PartReader<'_, 'a>> {
        let name = self
            .entries
            .get(&part.key)
            .with_context(|| format!("3mf has no part {part}"))?
            .clone();
        let limit = self.part_bytes;
        let file = self
            .zip
            .by_name(&name)
            .map_err(|e| anyhow!("3mf entry {part}: {e}"))?;
        Ok(BufReader::new(Capped {
            inner: file,
            left: limit,
            limit,
        }))
    }

    /// The root model: the start part `_rels/.rels` names, else [`DEFAULT_ROOT`].
    fn root(&mut self) -> Result<PartName> {
        let rels = PartName::new("_rels/.rels");
        let named = if self.has(&rels) {
            start_part_target(self.entry(&rels)?)?
        } else {
            None
        };
        if let Some(target) = named {
            let root = PartName::new(&target);
            if !self.has(&root) {
                bail!("3mf root model {root} (named by _rels/.rels) is missing");
            }
            return Ok(root);
        }
        let fallback = PartName::new(DEFAULT_ROOT);
        if self.has(&fallback) {
            return Ok(fallback);
        }
        bail!("3mf has no root model: _rels/.rels names none and {fallback} is missing")
    }

    /// Parse `root` and every part a parsed part's components (or the root's build items) name, each
    /// once. A part nothing names is never read, so a broken stray entry can't fail an import that
    /// doesn't use it. A named part that's MISSING isn't an error here: [`collect`] reports it if
    /// something actually instantiates it.
    fn load_reachable(&mut self, root: &PartName) -> Result<()> {
        let mut queue = vec![root.clone()];
        while let Some(at) = queue.pop() {
            if self.parts.contains_key(&at) || !self.has(&at) {
                continue;
            }
            let part = parse_part(&at, self.entry(&at)?)?;
            let components = part.objects.values().flat_map(|o| &o.components);
            let items = part.build.iter().filter(|_| at == *root);
            queue.extend(components.chain(items).filter_map(|i| i.path.clone()));
            self.parts.insert(at, part);
        }
        Ok(())
    }
}

/// Which vocabulary an element or attribute belongs to, decided by namespace URI, never by prefix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Vocab {
    /// 3MF core (any version). Also anything with NO namespace: an unprefixed attribute (XML-NS gives
    /// it none), or an element from a producer that never declared a default namespace.
    Core,
    /// The Materials extension, where most producers put `<m:basematerials>`.
    Materials,
    /// The Production extension (`p:path`).
    Production,
    /// Everything else: other extensions, vendor data, `xml:` attributes, `xmlns` declarations.
    Foreign,
}

fn vocab(ns: &ResolveResult) -> Vocab {
    const CORE: &[u8] = b"http://schemas.microsoft.com/3dmanufacturing/core/";
    const MATERIALS: &[u8] = b"http://schemas.microsoft.com/3dmanufacturing/material/";
    const PRODUCTION: &[u8] = b"http://schemas.microsoft.com/3dmanufacturing/production/";
    match ns {
        ResolveResult::Unbound => Vocab::Core,
        ResolveResult::Bound(uri) if uri.as_ref().starts_with(CORE) => Vocab::Core,
        ResolveResult::Bound(uri) if uri.as_ref().starts_with(MATERIALS) => Vocab::Materials,
        ResolveResult::Bound(uri) if uri.as_ref().starts_with(PRODUCTION) => Vocab::Production,
        _ => Vocab::Foreign,
    }
}

/// Whether an element (and its whole subtree) is outside what we read. Its children would otherwise
/// inherit the DEFAULT namespace, so an unprefixed `<vertex>` under a vendor element would read as core.
fn skipped(v: Vocab, local: &[u8]) -> bool {
    match v {
        Vocab::Core => false,
        Vocab::Materials => !matches!(local, b"basematerials" | b"base"),
        Vocab::Production | Vocab::Foreign => true,
    }
}

/// One part's decompressed bytes, capped (TF.11): a few MB of DEFLATE can expand to gigabytes of XML,
/// and quick-xml buffers a text node whole. Past the cap the read ERRORS; a quiet `take()` would end
/// the stream early, which reads as a clean, truncated part.
struct Capped<R> {
    inner: R,
    left: u64,
    limit: u64,
}

impl<R: std::io::Read> std::io::Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.left = self.left.checked_sub(n as u64).ok_or_else(|| {
            std::io::Error::other(format!(
                "decompresses past the {}-byte part limit",
                self.limit
            ))
        })?;
        Ok(n)
    }
}

/// The `Target` of the start-part relationship in a `.rels` part, if it has one.
fn start_part_target(src: impl BufRead) -> Result<Option<String>> {
    let rels = PartName::new("_rels/.rels");
    let mut reader = NsReader::from_reader(src);
    let mut buf = Vec::new();
    loop {
        let (_, event) = reader
            .read_resolved_event_into(&mut buf)
            .map_err(|e| anyhow!("3mf {rels}: {e}"))?;
        match event {
            Event::Eof => return Ok(None),
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Relationship" => {
                let a = Attrs {
                    name: &rels,
                    reader: &reader,
                    e: &e,
                };
                if a.get(Vocab::Core, "Type")?.as_deref() == Some(START_PART_REL) {
                    return a
                        .get(Vocab::Core, "Target")?
                        .context("3mf _rels/.rels: the start-part relationship has no Target")
                        .map(Some);
                }
            }
            _ => {}
        }
        buf.clear();
    }
}

/// Pull-parse one `.model` part, keeping only what placement and color need: `<metadata>` (wherever it
/// sits) and every extension's elements fall through untouched.
fn parse_part(name: &PartName, src: impl BufRead) -> Result<Part> {
    let mut reader = NsReader::from_reader(src);
    let mut buf = Vec::new();
    let mut st = PartState::default();
    // Every open element, whatever its vocabulary. quick-xml reports a clean Eof with elements still
    // open, so a part cut off inside <build> would otherwise drop its remaining items silently.
    let mut open = 0usize;
    // Depth inside a skipped subtree.
    let mut foreign = 0usize;
    loop {
        let (ns, event) = reader
            .read_resolved_event_into(&mut buf)
            .map_err(|e| anyhow!("3mf {name}: xml: {e}"))?;
        let v = vocab(&ns);
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                open += 1;
                if foreign > 0 || skipped(v, e.local_name().as_ref()) {
                    foreign += 1;
                } else {
                    st.open(&Attrs {
                        name,
                        reader: &reader,
                        e: &e,
                    })?;
                }
            }
            // Self-closing: `<object/>`, `<build/>` and `<mesh/>` must close as well as open.
            Event::Empty(e) => {
                if foreign == 0 && !skipped(v, e.local_name().as_ref()) {
                    st.open(&Attrs {
                        name,
                        reader: &reader,
                        e: &e,
                    })?;
                    st.close(name, e.local_name().as_ref())?;
                }
            }
            Event::End(e) => {
                open = open.saturating_sub(1);
                if foreign > 0 {
                    foreign -= 1;
                } else {
                    st.close(name, e.local_name().as_ref())?;
                }
            }
            _ => {}
        }
        buf.clear();
    }
    if open > 0 {
        bail!("3mf {name}: the XML ends with {open} element(s) still open (a truncated part?)");
    }
    Ok(st.part)
}

/// The parser's position in the document, beyond the [`Part`] it builds.
#[derive(Default)]
struct PartState {
    part: Part,
    object: Option<(u32, ObjectDef)>,
    mesh: Option<MeshDef>,
    group: Option<u32>,
    in_build: bool,
}

impl PartState {
    fn open<R>(&mut self, a: &Attrs<'_, '_, R>) -> Result<()> {
        match a.e.local_name().as_ref() {
            b"object" => {
                let def = ObjectDef {
                    kind: ObjectKind::parse(a.get(Vocab::Core, "type")?.as_deref()),
                    pid: a.opt_num("pid")?,
                    pindex: a.opt_num("pindex")?,
                    ..ObjectDef::default()
                };
                self.object = Some((a.req_num("id")?, def));
            }
            b"mesh" if self.object.is_some() => self.mesh = Some(MeshDef::default()),
            b"vertex" => {
                if let Some(mesh) = &mut self.mesh {
                    mesh.verts
                        .push([a.req_num("x")?, a.req_num("y")?, a.req_num("z")?]);
                }
            }
            b"triangle" => {
                if let Some(mesh) = &mut self.mesh {
                    mesh.tris
                        .push([a.req_num("v1")?, a.req_num("v2")?, a.req_num("v3")?]);
                }
            }
            b"component" => {
                if let Some((_, obj)) = &mut self.object {
                    obj.components.push(a.instance()?);
                }
            }
            b"build" => self.in_build = true,
            b"item" if self.in_build => self.part.build.push(a.instance()?),
            b"basematerials" => {
                let id = a.req_num("id")?;
                self.part.materials.entry(id).or_default();
                self.group = Some(id);
            }
            b"base" => {
                if let Some(id) = self.group {
                    // An entry without a color still holds its index, so later pindexes stay aligned.
                    let color = a.get(Vocab::Core, "displaycolor")?.unwrap_or_default();
                    self.part.materials.entry(id).or_default().push(color);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn close(&mut self, name: &PartName, local: &[u8]) -> Result<()> {
        match local {
            b"mesh" => {
                if let (Some(mesh), Some((id, obj))) = (self.mesh.take(), &mut self.object) {
                    let n = mesh.verts.len();
                    for (t, tri) in mesh.tris.iter().enumerate() {
                        if let Some(v) = tri.iter().find(|&&v| v as usize >= n) {
                            bail!(
                                "3mf {name}: object {id}: triangle {t} references vertex {v}, but the mesh has {n} vertices"
                            );
                        }
                    }
                    obj.mesh = Some(mesh);
                }
            }
            b"object" => {
                if let Some((id, obj)) = self.object.take() {
                    // First definition wins on a duplicated id, as the threemf-era reader's lookup did.
                    self.part.objects.entry(id).or_insert(obj);
                }
            }
            b"build" => self.in_build = false,
            b"basematerials" => self.group = None,
            _ => {}
        }
        Ok(())
    }
}

/// One element's attributes, with the reader that resolves their namespaces. A CORE attribute is
/// unprefixed and a Production one is `p:`-bound; matching on that, not on the local name alone, keeps
/// `xml:id`, an `xmlns:path` declaration or a vendor's `x:objectid` from standing in for the real one.
struct Attrs<'r, 'e, R> {
    name: &'r PartName,
    reader: &'r NsReader<R>,
    e: &'r BytesStart<'e>,
}

impl<R> Attrs<'_, '_, R> {
    fn tag(&self) -> String {
        String::from_utf8_lossy(self.e.local_name().as_ref()).into_owned()
    }

    fn get(&self, want: Vocab, key: &str) -> Result<Option<String>> {
        for a in self.e.attributes() {
            let a = a.map_err(|err| anyhow!("3mf {}: <{}>: {err}", self.name, self.tag()))?;
            let (ns, local) = self.reader.resolver().resolve_attribute(a.key);
            if local.as_ref() == key.as_bytes() && vocab(&ns) == want {
                let v = a
                    .unescape_value()
                    .map_err(|err| anyhow!("3mf {}: <{}> {key}: {err}", self.name, self.tag()))?;
                return Ok(Some(v.into_owned()));
            }
        }
        Ok(None)
    }

    fn opt_num<T: std::str::FromStr>(&self, key: &str) -> Result<Option<T>> {
        self.get(Vocab::Core, key)?
            .map(|s| {
                s.trim().parse::<T>().map_err(|_| {
                    anyhow!(
                        "3mf {}: <{}> {key}='{s}' is not a valid number",
                        self.name,
                        self.tag()
                    )
                })
            })
            .transpose()
    }

    fn req_num<T: std::str::FromStr>(&self, key: &str) -> Result<T> {
        self.opt_num(key)?
            .with_context(|| format!("3mf {}: <{}> has no {key}", self.name, self.tag()))
    }

    /// A build `<item>` or `<component>`: `objectid`, optional `transform`, optional `p:path`.
    fn instance(&self) -> Result<Instance> {
        let transform = match self.get(Vocab::Core, "transform")? {
            Some(s) => Some(
                parse_transform(&s)
                    .with_context(|| format!("3mf {}: <{}> transform", self.name, self.tag()))?,
            ),
            None => None,
        };
        Ok(Instance {
            path: self
                .get(Vocab::Production, "path")?
                .map(|p| PartName::new(&p)),
            objectid: self.req_num("objectid")?,
            transform,
        })
    }
}

/// The 3MF transform attribute: exactly 12 numbers, `m00 m01 m02 m10 … m22 tx ty tz`.
fn parse_transform(s: &str) -> Result<[f64; 12]> {
    let nums = s
        .split_whitespace()
        .map(|t| {
            t.parse::<f64>()
                .map_err(|_| anyhow!("'{t}' is not a number"))
        })
        .collect::<Result<Vec<_>>>()?;
    <[f64; 12]>::try_from(nums.as_slice())
        .map_err(|_| anyhow!("needs 12 numbers, got {}", nums.len()))
}

/// Part `at`'s object `oid`, or an error naming whichever of the two is missing.
fn lookup<'p>(
    parts: &'p HashMap<PartName, Part>,
    at: &PartName,
    oid: u32,
) -> Result<(&'p Part, &'p ObjectDef)> {
    let part = parts.get(at).with_context(|| {
        format!("3mf references model part {at}, which the package doesn't have")
    })?;
    let obj = part.objects.get(&oid).with_context(|| {
        format!("3mf references object {oid} in {at}, which has no such object")
    })?;
    Ok((part, obj))
}

/// Triangles object `oid` of part `at` expands to with its components instanced: the cost of
/// [`collect`] before it runs. Memoized per object (a shared sub-assembly is counted once, then
/// multiplied by its references), saturating so a hostile count tops out instead of wrapping, and
/// walking with the current path so a cycle is named exactly instead of guessed from depth.
fn expanded_tris(
    parts: &HashMap<PartName, Part>,
    at: &PartName,
    oid: u32,
    memo: &mut HashMap<(PartName, u32), u64>,
    path: &mut Vec<(PartName, u32)>,
) -> Result<u64> {
    let key = (at.clone(), oid);
    if let Some(&n) = memo.get(&key) {
        return Ok(n);
    }
    if path.contains(&key) {
        bail!("3mf component cycle: object {oid} in {at} contains itself");
    }
    if path.len() >= MAX_DEPTH {
        bail!("3mf component nesting deeper than {MAX_DEPTH} levels at object {oid} in {at}");
    }
    let (_, obj) = lookup(parts, at, oid)?;
    let mut n = 0u64;
    if obj.kind.printable() {
        path.push(key.clone());
        n = obj.mesh.as_ref().map_or(0, |m| m.tris.len() as u64);
        for c in &obj.components {
            let target = c.path.as_ref().unwrap_or(at);
            n = n.saturating_add(expanded_tris(parts, target, c.objectid, memo, path)?);
        }
        path.pop();
    }
    memo.insert(key, n);
    Ok(n)
}

/// Instantiate object `oid` of part `at` under `xf`, depth-first: its mesh (if any), then each
/// component. A non-printable object (`other`, `support`) contributes nothing, subtree included.
fn collect(
    parts: &HashMap<PartName, Part>,
    at: &PartName,
    oid: u32,
    xf: Option<[f64; 12]>,
    out: &mut Vec<Object3mf>,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("3mf component nesting deeper than {MAX_DEPTH} levels at object {oid} in {at}");
    }
    let (part, obj) = lookup(parts, at, oid)?;
    if !obj.kind.printable() {
        return Ok(());
    }
    if let Some(mesh) = &obj.mesh {
        // A mirrored placement (negative determinant) turns the mesh inside out, and the kernel takes
        // that as a negative-volume solid rather than rejecting it, so reverse each triangle. OpenSCAD
        // 2026.06 imports it inside out; accepted divergence (chotchki, TF.10), in the spec.
        let tris = if det3(xf) < 0.0 {
            mesh.tris.iter().map(|&[a, b, c]| [a, c, b]).collect()
        } else {
            mesh.tris.clone()
        };
        out.push(Object3mf {
            verts: mesh.verts.iter().map(|&v| apply(xf, v)).collect(),
            tris,
            color: color_of(&part.materials, obj),
        });
    }
    for c in &obj.components {
        // The 3MF spec: a component's own transform applies first, then its parent's (row vectors:
        // `compose(parent, child)`), and an absent or identity transform moves nothing. OpenSCAD
        // diverges on both counts (2026.06): `import_3mf_v2.cc:135` gives a component lib3mf calls
        // untransformed (an explicit identity included) a (0,0,1) translation, and `:145` composes
        // `cm * m`, the child AFTER its parent. Accepted, not chased (chotchki, TF): the `import/3mf`
        // spec records the oracle's numbers, so a `fab render --check` hit on a component-bearing 3MF
        // is this divergence, not a regression.
        let target = c.path.as_ref().unwrap_or(at);
        collect(
            parts,
            target,
            c.objectid,
            compose(xf, c.transform),
            out,
            depth + 1,
        )?;
    }
    Ok(())
}

/// Determinant of a 3MF transform's linear part; an absent transform is the identity.
fn det3(xf: Option<[f64; 12]>) -> f64 {
    let Some(m) = xf else { return 1.0 };
    m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
        + m[2] * (m[3] * m[7] - m[4] * m[6])
}

/// 3MF transform: 12 values `m00..m22 tx ty tz`, ROW-vector convention —
/// `x' = x*m00 + y*m10 + z*m20 + tx`.
fn apply(xf: Option<[f64; 12]>, p: [f64; 3]) -> [f64; 3] {
    let Some(m) = xf else { return p };
    std::array::from_fn(|i| p[0] * m[i] + p[1] * m[3 + i] + p[2] * m[6 + i] + m[9 + i])
}

/// `outer ∘ inner` in the same row-vector convention (inner applies first).
fn compose(outer: Option<[f64; 12]>, inner: Option<[f64; 12]>) -> Option<[f64; 12]> {
    match (outer, inner) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => {
            let mut m = [0.0; 12];
            for i in 0..3 {
                for j in 0..3 {
                    m[i * 3 + j] = (0..3).map(|k| b[i * 3 + k] * a[k * 3 + j]).sum();
                }
            }
            for j in 0..3 {
                m[9 + j] = (0..3).map(|k| b[9 + k] * a[k * 3 + j]).sum::<f64>() + a[9 + j];
            }
            Some(m)
        }
    }
}

/// The object's basematerial color: pid → its part's group, pindex → entry, `#RRGGBB[AA]`.
fn color_of(materials: &HashMap<u32, Vec<String>>, obj: &ObjectDef) -> Option<[f32; 4]> {
    let group = materials.get(&obj.pid?)?;
    parse_color(group.get(obj.pindex.unwrap_or(0))?)
}

fn parse_color(s: &str) -> Option<[f32; 4]> {
    let h = s.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    let (r, g, b) = (byte(0)?, byte(2)?, byte(4)?);
    let a = if h.len() >= 8 { byte(6)? } else { 255 };
    Some([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ])
}

/// Synthetic packages for tests here and in `geomsvc` (real-world 3MFs are never committed: MakerWorld
/// files carry their own license and `models/` is its own repo).
#[cfg(test)]
pub(crate) mod fixtures {
    use std::io::Write;

    pub(crate) const CORE: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
    pub(crate) const PROD: &str = "http://schemas.microsoft.com/3dmanufacturing/production/2015/06";

    /// A model part around `body`, with the core namespace default and `p:` bound to Production.
    pub(crate) fn model(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><model unit="millimeter" xmlns="{CORE}" xmlns:p="{PROD}">{body}</model>"#
        )
    }

    /// The unit cube [0,1]³ as a `<mesh>`: vertex `x + 2y + 4z`, 12 outward triangles.
    pub(crate) fn cube_mesh() -> String {
        let mut s = String::from("<mesh><vertices>");
        for z in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    s += &format!(r#"<vertex x="{x}" y="{y}" z="{z}"/>"#);
                }
            }
        }
        s += "</vertices><triangles>";
        for [a, b, c] in [
            [0, 2, 1],
            [1, 2, 3],
            [4, 5, 6],
            [5, 7, 6],
            [0, 1, 4],
            [1, 5, 4],
            [2, 6, 3],
            [3, 6, 7],
            [0, 4, 2],
            [2, 4, 6],
            [1, 3, 5],
            [3, 7, 5],
        ] {
            s += &format!(r#"<triangle v1="{a}" v2="{b}" v3="{c}"/>"#);
        }
        s + "</triangles></mesh>"
    }

    /// `_rels/.rels` naming `target` as the start part.
    pub(crate) fn rels(target: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="{target}" Id="rel0" Type="{}"/></Relationships>"#,
            super::START_PART_REL
        )
    }

    /// Zip `entries` (name, contents), every one with `method`.
    pub(crate) fn package(entries: &[(&str, &str)], method: zip::CompressionMethod) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default().compression_method(method);
            for (name, body) in entries {
                z.start_file(*name, o).unwrap();
                z.write_all(body.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    /// [`package`] with every entry Stored.
    pub(crate) fn stored(entries: &[(&str, &str)]) -> Vec<u8> {
        package(entries, zip::CompressionMethod::Stored)
    }

    /// A one-part package: the unit cube as object 1, placed through a components object 2 whose
    /// component carries `comp_xf` (None = no attribute), under one build item carrying `item_xf`.
    pub(crate) fn cube_through_component(comp_xf: Option<&str>, item_xf: Option<&str>) -> Vec<u8> {
        let ca = comp_xf
            .map(|t| format!(r#" transform="{t}""#))
            .unwrap_or_default();
        let ia = item_xf
            .map(|t| format!(r#" transform="{t}""#))
            .unwrap_or_default();
        let body = format!(
            r#"<resources><object id="1" type="model">{}</object><object id="2" type="model"><components><component objectid="1"{ca}/></components></object></resources><build><item objectid="2"{ia}/></build>"#,
            cube_mesh()
        );
        stored(&[("3D/3dmodel.model", &model(&body))])
    }

    /// The Bambu Studio / MakerWorld shape: the root's object 2 is one component whose `p:path` names
    /// `3D/Objects/object_1.model`, which holds the cube as object 1 and an empty `<build/>`.
    pub(crate) fn production_cube(method: zip::CompressionMethod) -> Vec<u8> {
        let root = model(
            r#"<metadata name="Application">BambuStudio</metadata><resources><object id="2" type="model"><components><component p:path="/3D/Objects/object_1.model" objectid="1" transform="1 0 0 0 1 0 0 0 1 0 0 0"/></components></object></resources><build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 10 20 30"/></build><metadata name="ProfileTitle">after the build</metadata>"#,
        );
        let sub = model(&format!(
            r#"<metadata name="BambuStudio:3mfVersion">1</metadata><resources><object id="1" type="model">{}</object></resources><build/>"#,
            cube_mesh()
        ));
        package(
            &[
                ("_rels/.rels", &rels("/3D/3dmodel.model")),
                ("3D/3dmodel.model", &root),
                ("3D/Objects/object_1.model", &sub),
            ],
            method,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{
        cube_mesh, cube_through_component, model, production_cube, rels, stored,
    };
    use super::*;

    /// Axis-aligned bounds over every vertex of every object.
    fn bbox(objs: &[Object3mf]) -> ([f64; 3], [f64; 3]) {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for v in objs.iter().flat_map(|o| &o.verts) {
            for i in 0..3 {
                lo[i] = lo[i].min(v[i]);
                hi[i] = hi[i].max(v[i]);
            }
        }
        (lo, hi)
    }

    fn assert_bbox(objs: &[Object3mf], lo: [f64; 3], hi: [f64; 3]) {
        let (l, h) = bbox(objs);
        for i in 0..3 {
            assert!(
                (l[i] - lo[i]).abs() < 1e-9 && (h[i] - hi[i]).abs() < 1e-9,
                "bbox {l:?}..{h:?}, want {lo:?}..{hi:?}"
            );
        }
    }

    fn err(bytes: &[u8]) -> String {
        match parse_3mf(bytes) {
            Ok(_) => panic!("expected an error"),
            Err(e) => format!("{e:#}"),
        }
    }

    // --- TF.2: the package reader ---

    #[test]
    fn metadata_on_both_sides_of_the_build_reads() {
        // The reported error: Bambu writes <metadata> before <resources> AND after </build>, which the
        // threemf-era reader failed as "duplicate field `metadata`".
        let bytes = stored(&[(
            "3D/3dmodel.model",
            &model(&format!(
                r#"<metadata name="Title">before</metadata><resources><object id="1">{}</object></resources><build><item objectid="1"/></build><metadata name="CopyRight">after</metadata>"#,
                cube_mesh()
            )),
        )]);
        let objs = parse_3mf(&bytes).unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].tris.len(), 12);
    }

    #[test]
    fn the_root_is_the_start_part_rels_names() {
        let main = model(&format!(
            r#"<resources><object id="1">{}</object></resources><build><item objectid="1" transform="1 0 0 0 1 0 0 0 1 5 0 0"/></build>"#,
            cube_mesh()
        ));
        let bytes = stored(&[
            ("_rels/.rels", &rels("/3D/main.model")),
            ("3D/main.model", &main),
        ]);
        assert_bbox(
            &parse_3mf(&bytes).unwrap(),
            [5.0, 0.0, 0.0],
            [6.0, 1.0, 1.0],
        );
    }

    #[test]
    fn without_rels_the_root_is_3d_3dmodel() {
        let bytes = cube_through_component(None, None);
        assert_eq!(parse_3mf(&bytes).unwrap().len(), 1);
    }

    #[test]
    fn a_rels_start_part_that_is_missing_is_named() {
        let bytes = stored(&[("_rels/.rels", &rels("/3D/gone.model"))]);
        assert!(err(&bytes).contains("/3D/gone.model"), "{}", err(&bytes));
    }

    #[test]
    fn deflate_compressed_entries_read() {
        let objs = parse_3mf(&production_cube(zip::CompressionMethod::Deflated)).unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].tris.len(), 12);
    }

    #[test]
    fn a_short_transform_is_an_error_naming_the_element() {
        let bytes = cube_through_component(Some("1 0 0 0 1 0 0 0 1"), None);
        let e = err(&bytes);
        assert!(
            e.contains("<component> transform") && e.contains("12 numbers, got 9"),
            "{e}"
        );
    }

    #[test]
    fn a_non_numeric_coordinate_is_an_error_naming_the_element() {
        let bytes = stored(&[(
            "3D/3dmodel.model",
            &model(
                r#"<resources><object id="1"><mesh><vertices><vertex x="1" y="oops" z="0"/></vertices><triangles/></mesh></object></resources><build><item objectid="1"/></build>"#,
            ),
        )]);
        let e = err(&bytes);
        assert!(e.contains("<vertex> y='oops'"), "{e}");
    }

    #[test]
    fn the_model_unit_does_not_scale() {
        // The oracle ignores `unit` too; inches stay 1, not 25.4.
        let body = format!(
            r#"<resources><object id="1">{}</object></resources><build><item objectid="1"/></build>"#,
            cube_mesh()
        );
        let xml = model(&body).replace(r#"unit="millimeter""#, r#"unit="inch""#);
        assert_bbox(
            &parse_3mf(&stored(&[("3D/3dmodel.model", &xml)])).unwrap(),
            [0.0; 3],
            [1.0; 3],
        );
    }

    // --- TF.3: resolution and placement ---
    //
    // A–D are the spec's scenarios, probed on the OpenSCAD 2026.06.12 binary. Its numbers are in each
    // comment; ours follow the 3MF spec (see `collect`).

    #[test]
    fn a_a_component_without_a_transform_moves_nothing() {
        // OpenSCAD: z [1,2] — its (0,0,1) default for an untransformed component.
        assert_bbox(
            &parse_3mf(&cube_through_component(None, None)).unwrap(),
            [0.0; 3],
            [1.0; 3],
        );
    }

    #[test]
    fn b_an_identity_component_transform_moves_nothing() {
        // OpenSCAD: z [1,2] — lib3mf reports an explicit identity as "no transform".
        let bytes = cube_through_component(Some("1 0 0 0 1 0 0 0 1 0 0 0"), None);
        assert_bbox(&parse_3mf(&bytes).unwrap(), [0.0; 3], [1.0; 3]);
    }

    #[test]
    fn c_a_translated_component_agrees_with_the_oracle() {
        // OpenSCAD: z [0.5,1.5] too — a non-identity transform skips its default.
        let bytes = cube_through_component(Some("1 0 0 0 1 0 0 0 1 0 0 0.5"), None);
        assert_bbox(
            &parse_3mf(&bytes).unwrap(),
            [0.0, 0.0, 0.5],
            [1.0, 1.0, 1.5],
        );
    }

    #[test]
    fn d_the_component_transform_applies_before_its_parents() {
        // Item: a quarter turn about X (y' = -z, z' = y); component: +5 in Z. Component first, so the
        // offset turns with the part onto -Y. OpenSCAD composes child after parent: y [-1,0], z [5,6].
        let bytes = cube_through_component(
            Some("1 0 0 0 1 0 0 0 1 0 0 5"),
            Some("1 0 0 0 0 1 0 -1 0 0 0 0"),
        );
        assert_bbox(
            &parse_3mf(&bytes).unwrap(),
            [0.0, -6.0, 0.0],
            [1.0, -5.0, 1.0],
        );
    }

    #[test]
    fn a_component_resolves_in_the_part_its_path_names() {
        let objs = parse_3mf(&production_cube(zip::CompressionMethod::Stored)).unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].tris.len(), 12);
        assert_bbox(&objs, [10.0, 20.0, 30.0], [11.0, 21.0, 31.0]);
    }

    #[test]
    fn part_names_match_case_insensitively() {
        let root = model(
            r#"<resources><object id="2"><components><component p:path="/3d/OBJECTS/Object_1.MODEL" objectid="1"/></components></object></resources><build><item objectid="2"/></build>"#,
        );
        let sub = model(&format!(
            r#"<resources><object id="1">{}</object></resources><build/>"#,
            cube_mesh()
        ));
        let bytes = stored(&[
            ("3D/3dmodel.model", &root),
            ("3D/Objects/object_1.model", &sub),
        ]);
        assert_eq!(parse_3mf(&bytes).unwrap().len(), 1);
    }

    #[test]
    fn a_sub_model_build_adds_nothing() {
        let root = model(
            r#"<resources><object id="2"><components><component p:path="/3D/Objects/sub.model" objectid="1"/></components></object></resources><build><item objectid="2"/></build>"#,
        );
        // The sub-model's own build would add a second, shifted cube if it counted.
        let sub = model(&format!(
            r#"<resources><object id="1">{}</object></resources><build><item objectid="1" transform="1 0 0 0 1 0 0 0 1 100 0 0"/></build>"#,
            cube_mesh()
        ));
        let bytes = stored(&[("3D/3dmodel.model", &root), ("3D/Objects/sub.model", &sub)]);
        let objs = parse_3mf(&bytes).unwrap();
        assert_eq!(objs.len(), 1);
        assert_bbox(&objs, [0.0; 3], [1.0; 3]);
    }

    #[test]
    fn color_resolves_in_the_part_that_declares_the_object() {
        // Group id 5 exists in BOTH parts with different colors; the object's own part wins.
        let root = model(
            r##"<resources><m:basematerials xmlns:m="http://schemas.microsoft.com/3dmanufacturing/material/2015/02" id="5"><m:base displaycolor="#FF0000"/></m:basematerials><object id="2"><components><component p:path="/3D/Objects/sub.model" objectid="1"/></components></object></resources><build><item objectid="2"/></build>"##,
        );
        let sub = model(&format!(
            r##"<resources><basematerials id="5"><base displaycolor="#0000FF"/></basematerials><object id="1" pid="5" pindex="0">{}</object></resources><build/>"##,
            cube_mesh()
        ));
        let bytes = stored(&[("3D/3dmodel.model", &root), ("3D/Objects/sub.model", &sub)]);
        let c = parse_3mf(&bytes).unwrap()[0].color.unwrap();
        assert!(c[2] > c[0], "expected the sub-model's blue, got {c:?}");
    }

    #[test]
    fn a_missing_part_is_named() {
        let root = model(
            r#"<resources><object id="2"><components><component p:path="/3D/Objects/missing.model" objectid="1"/></components></object></resources><build><item objectid="2"/></build>"#,
        );
        let e = err(&stored(&[("3D/3dmodel.model", &root)]));
        assert!(e.contains("/3D/Objects/missing.model"), "{e}");
    }

    #[test]
    fn a_missing_object_is_named_with_its_part() {
        let bytes = stored(&[(
            "3D/3dmodel.model",
            &model(r#"<resources/><build><item objectid="7"/></build>"#),
        )]);
        let e = err(&bytes);
        assert!(
            e.contains("object 7") && e.contains("/3D/3dmodel.model"),
            "{e}"
        );
    }

    #[test]
    fn an_out_of_range_triangle_index_is_named() {
        let bytes = stored(&[(
            "3D/3dmodel.model",
            &model(
                r#"<resources><object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/><vertex x="0" y="0" z="1"/></vertices><triangles><triangle v1="0" v2="1" v3="9"/></triangles></mesh></object></resources><build><item objectid="1"/></build>"#,
            ),
        )]);
        let e = err(&bytes);
        assert!(e.contains("vertex 9") && e.contains("4 vertices"), "{e}");
    }

    #[test]
    fn a_component_cycle_fails_instead_of_hanging() {
        let bytes = stored(&[(
            "3D/3dmodel.model",
            &model(
                r#"<resources><object id="1"><components><component objectid="2"/></components></object><object id="2"><components><component objectid="1"/></components></object></resources><build><item objectid="1"/></build>"#,
            ),
        )]);
        assert!(err(&bytes).contains("cycle"), "{}", err(&bytes));
    }

    // --- review findings (TF): namespaces, truncation, and the resolution shapes ---

    fn one_part(body: &str) -> Vec<u8> {
        stored(&[("3D/3dmodel.model", &model(body))])
    }

    #[test]
    fn a_foreign_mesh_inside_an_object_does_not_wipe_the_real_one() {
        let c = cube_mesh();
        for body in [
            format!(
                r#"<resources><object id="1">{c}<x:mesh xmlns:x="urn:acme:lod"/></object></resources><build><item objectid="1"/></build>"#
            ),
            format!(
                r#"<resources><object id="1">{}<x:mesh xmlns:x="urn:acme"/></mesh></object></resources><build><item objectid="1"/></build>"#,
                c.trim_end_matches("</mesh>")
            ),
        ] {
            let objs = parse_3mf(&one_part(&body)).unwrap();
            assert_eq!(objs[0].tris.len(), 12, "{body}");
        }
    }

    #[test]
    fn a_vendor_subtree_is_skipped_whole_even_its_default_namespace_children() {
        // `<object>` inside a rebound default namespace is vendor data, not a second object 1.
        let body = format!(
            r#"<resources><ext xmlns="urn:acme"><object id="1"/></ext><object id="1">{}</object></resources><build><item objectid="1"/></build>"#,
            cube_mesh()
        );
        assert_eq!(parse_3mf(&one_part(&body)).unwrap()[0].tris.len(), 12);
        // A vendor <x:item> nested in a build item doesn't add an instance.
        let body = format!(
            r#"<resources><object id="1">{}</object></resources><build><item objectid="1"><x:item xmlns:x="urn:x" objectid="1" transform="1 0 0 0 1 0 0 0 1 50 0 0"/></item></build>"#,
            cube_mesh()
        );
        assert_eq!(parse_3mf(&one_part(&body)).unwrap().len(), 1);
    }

    #[test]
    fn only_unprefixed_attributes_count_as_core() {
        // `xml:id` is not the object's id; the threemf-era reader read this file correctly too.
        let body = format!(
            r#"<resources><object xml:id="bracket" id="1">{}</object></resources><build><item objectid="1"/></build>"#,
            cube_mesh()
        );
        assert_eq!(parse_3mf(&one_part(&body)).unwrap().len(), 1);
        // An `xmlns:path` DECLARATION is not a component's p:path.
        let root = model(
            r#"<resources><object id="2"><components><component xmlns:path="urn:x" p:path="/3D/Objects/object_1.model" objectid="1"/></components></object></resources><build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 10 20 30"/></build>"#,
        );
        let sub = model(&format!(
            r#"<resources><object id="1">{}</object></resources><build/>"#,
            cube_mesh()
        ));
        let bytes = stored(&[
            ("3D/3dmodel.model", &root),
            ("3D/Objects/object_1.model", &sub),
        ]);
        assert_bbox(
            &parse_3mf(&bytes).unwrap(),
            [10.0, 20.0, 30.0],
            [11.0, 21.0, 31.0],
        );
    }

    #[test]
    fn a_root_model_cut_off_inside_its_build_is_an_error() {
        let full = model(&format!(
            r#"<resources><object id="1">{m}</object><object id="2">{m}</object></resources><build><item objectid="1"/><item objectid="2" transform="1 0 0 0 1 0 0 0 1 5 0 0"/></build>"#,
            m = cube_mesh()
        ));
        // Cut cleanly after the first item: no `</build></model>`, and object 2 never instantiated.
        let cut = &full[..full.find(r#"<item objectid="2""#).unwrap()];
        let e = err(&stored(&[("3D/3dmodel.model", cut)]));
        assert!(e.contains("still open"), "{e}");
        // Cutting mid-tag was already an error (quick-xml's own); keep it one.
        let mid = &full[..full.find(r#"objectid="2" transform"#).unwrap()];
        assert!(parse_3mf(&stored(&[("3D/3dmodel.model", mid)])).is_err());
    }

    #[test]
    fn a_pathless_component_in_a_sub_model_resolves_in_that_sub_model() {
        // The root ALSO has an object 5, scaled 2x: a lookup in the wrong part gives wrong geometry,
        // not an error, so the bbox is what pins it.
        let root = model(&format!(
            r#"<resources><object id="9">{}</object><object id="5"><components><component objectid="9" transform="2 0 0 0 2 0 0 0 2 0 0 0"/></components></object><object id="2"><components><component p:path="/3D/Objects/sub.model" objectid="6"/></components></object></resources><build><item objectid="2"/></build>"#,
            cube_mesh()
        ));
        let sub = model(&format!(
            r#"<resources><object id="5">{}</object><object id="6"><components><component objectid="5"/></components></object></resources><build/>"#,
            cube_mesh()
        ));
        let bytes = stored(&[("3D/3dmodel.model", &root), ("3D/Objects/sub.model", &sub)]);
        assert_bbox(&parse_3mf(&bytes).unwrap(), [0.0; 3], [1.0; 3]);
    }

    #[test]
    fn a_build_item_resolves_in_the_part_its_path_names() {
        let root = model(
            r#"<resources/><build><item objectid="1" p:path="/3D/Objects/sub.model"/></build>"#,
        );
        let sub = model(&format!(
            r#"<resources><object id="1">{}</object></resources><build/>"#,
            cube_mesh()
        ));
        let bytes = stored(&[("3D/3dmodel.model", &root), ("3D/Objects/sub.model", &sub)]);
        let objs = parse_3mf(&bytes).unwrap();
        assert_eq!((objs.len(), objs[0].tris.len()), (1, 12));
        assert_bbox(&objs, [0.0; 3], [1.0; 3]);
    }

    #[test]
    fn a_component_naming_a_missing_object_names_the_sub_part() {
        let root = model(
            r#"<resources><object id="2"><components><component p:path="/3D/Objects/sub.model" objectid="7"/></components></object></resources><build><item objectid="2"/></build>"#,
        );
        let sub = model(&format!(
            r#"<resources><object id="1">{}</object></resources><build/>"#,
            cube_mesh()
        ));
        let e = err(&stored(&[
            ("3D/3dmodel.model", &root),
            ("3D/Objects/sub.model", &sub),
        ]));
        assert!(
            e.contains("object 7") && e.contains("/3D/Objects/sub.model"),
            "{e}"
        );
    }

    // --- TF.9: only printable objects contribute ---

    #[test]
    fn a_modifier_volume_reached_through_a_component_adds_nothing() {
        // OpenSCAD 2026.06 imports every mesh whatever its type: z [0,6] here.
        let body = format!(
            r#"<resources><object id="1" type="model">{c}</object><object id="3" type="other">{c}</object><object id="2" type="model"><components><component objectid="1"/><component objectid="3" transform="1 0 0 0 1 0 0 0 1 0 0 5"/></components></object></resources><build><item objectid="2"/></build>"#,
            c = cube_mesh()
        );
        let objs = parse_3mf(&one_part(&body)).unwrap();
        assert_eq!(objs.len(), 1);
        assert_bbox(&objs, [0.0; 3], [1.0; 3]);
    }

    #[test]
    fn support_is_skipped_but_solidsupport_and_surface_import() {
        let item = |kind: &str| {
            one_part(&format!(
                r#"<resources><object id="1" type="{kind}">{}</object></resources><build><item objectid="1"/></build>"#,
                cube_mesh()
            ))
        };
        assert!(err(&item("support")).contains("no printable meshes"));
        assert!(err(&item("other")).contains("no printable meshes"));
        assert_eq!(parse_3mf(&item("solidsupport")).unwrap().len(), 1);
        assert_eq!(parse_3mf(&item("surface")).unwrap().len(), 1);
    }

    // --- TF.10: mirrored placements import outward-facing ---

    /// Signed volume by the divergence theorem: +1 for an outward unit cube, -1 inside out.
    fn signed_volume(objs: &[Object3mf]) -> f64 {
        objs.iter()
            .flat_map(|o| o.tris.iter().map(move |t| t.map(|i| o.verts[i as usize])))
            .map(|[a, b, c]| {
                (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0]))
                    / 6.0
            })
            .sum()
    }

    #[test]
    fn a_mirrored_build_item_imports_outward_facing() {
        // OpenSCAD 2026.06: signed volume -1 (inside out).
        let body = format!(
            r#"<resources><object id="1">{}</object></resources><build><item objectid="1" transform="-1 0 0 0 1 0 0 0 1 0 0 0"/></build>"#,
            cube_mesh()
        );
        let objs = parse_3mf(&one_part(&body)).unwrap();
        assert_bbox(&objs, [-1.0, 0.0, 0.0], [0.0, 1.0, 1.0]);
        assert!(
            (signed_volume(&objs) - 1.0).abs() < 1e-9,
            "{}",
            signed_volume(&objs)
        );
    }

    #[test]
    fn a_mirror_at_the_component_imports_outward_facing_too() {
        let objs = parse_3mf(&cube_through_component(
            Some("-1 0 0 0 1 0 0 0 1 0 0 0"),
            None,
        ))
        .unwrap();
        assert!(
            (signed_volume(&objs) - 1.0).abs() < 1e-9,
            "{}",
            signed_volume(&objs)
        );
        // A double mirror is a rotation: no flip, still outward.
        let objs = parse_3mf(&cube_through_component(
            Some("-1 0 0 0 1 0 0 0 1 0 0 0"),
            Some("-1 0 0 0 1 0 0 0 1 0 0 0"),
        ))
        .unwrap();
        assert!(
            (signed_volume(&objs) - 1.0).abs() < 1e-9,
            "{}",
            signed_volume(&objs)
        );
    }

    // --- TF.11: resource limits ---

    #[test]
    fn exponential_instancing_fails_fast_naming_the_limit() {
        // Nine levels, ten references each, over a 12-triangle cube: 1.2e9 triangles in a few KB.
        let mut res = format!(r#"<object id="1">{}</object>"#, cube_mesh());
        for k in 2..=9 {
            res += &format!(r#"<object id="{k}"><components>"#);
            for _ in 0..10 {
                res += &format!(r#"<component objectid="{}"/>"#, k - 1);
            }
            res += "</components></object>";
        }
        let bytes = one_part(&format!(
            r#"<resources>{res}</resources><build><item objectid="9"/></build>"#
        ));
        let started = std::time::Instant::now();
        let e = err(&bytes);
        assert!(e.contains("50000000-triangle limit"), "{e}");
        assert!(
            started.elapsed().as_secs() < 5,
            "the cost check must not build the geometry"
        );
    }

    #[test]
    fn the_triangle_limit_counts_instances_not_definitions() {
        // One 12-triangle cube referenced twice is 24 triangles of build.
        let body = format!(
            r#"<resources><object id="1">{}</object></resources><build><item objectid="1"/><item objectid="1" transform="1 0 0 0 1 0 0 0 1 5 0 0"/></build>"#,
            cube_mesh()
        );
        let limits = |triangles| Limits {
            triangles,
            part_bytes: LIMITS.part_bytes,
        };
        assert!(parse_3mf_with(&one_part(&body), limits(23)).is_err());
        assert_eq!(
            parse_3mf_with(&one_part(&body), limits(24)).unwrap().len(),
            2
        );
    }

    #[test]
    fn deep_but_legal_nesting_imports() {
        // Twelve single-component levels: past the old depth-8 guess, nowhere near a cycle.
        let mut res = format!(r#"<object id="1">{}</object>"#, cube_mesh());
        for k in 2..=13 {
            res += &format!(
                r#"<object id="{k}"><components><component objectid="{}"/></components></object>"#,
                k - 1
            );
        }
        let bytes = one_part(&format!(
            r#"<resources>{res}</resources><build><item objectid="13"/></build>"#
        ));
        assert_bbox(&parse_3mf(&bytes).unwrap(), [0.0; 3], [1.0; 3]);
    }

    #[test]
    fn a_part_past_the_byte_cap_is_an_error_not_a_truncation() {
        let bytes = production_cube(zip::CompressionMethod::Deflated);
        let small = Limits {
            triangles: LIMITS.triangles,
            part_bytes: 600,
        };
        let e = match parse_3mf_with(&bytes, small) {
            Ok(_) => panic!("the sub-model is over 600 bytes decompressed"),
            Err(e) => format!("{e:#}"),
        };
        assert!(e.contains("600-byte part limit"), "{e}");
        assert!(parse_3mf_with(&bytes, LIMITS).is_ok());
    }

    // --- kept from the threemf era: these pass unchanged ---

    #[test]
    fn reads_colors_from_a_prefixed_materials_table() {
        // Hand-built two-object 3mf with an <m:basematerials> table — the exact shape threemf's
        // own serde fails to read back (the reason read_materials exists).
        let model = r##"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" xmlns:m="http://schemas.microsoft.com/3dmanufacturing/material/2015/02">
 <resources>
  <m:basematerials id="1"><m:base name="red" displaycolor="#D03030"/><m:base name="blue" displaycolor="#3060D0"/></m:basematerials>
  <object id="1" pid="1" pindex="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/><vertex x="0" y="0" z="1"/></vertices><triangles><triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3"/><triangle v1="1" v2="2" v3="3"/><triangle v1="0" v2="3" v3="2"/></triangles></mesh></object>
 </resources>
 <build><item objectid="1"/></build>
</model>"##;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            use std::io::Write;
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("3D/3dmodel.model", o).unwrap();
            z.write_all(model.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        let objs = parse_3mf(&buf.into_inner()).unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].tris.len(), 4);
        // pindex 1 = blue
        let c = objs[0].color.unwrap();
        assert!(c[2] > c[0], "expected blue-dominant, got {c:?}");
    }

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#FF8000"), Some([1.0, 128.0 / 255.0, 0.0, 1.0]));
        assert_eq!(
            parse_color("#00000080").map(|c| (c[3] * 255.0) as u8),
            Some(128)
        );
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn roundtrips_a_kernel_3mf_with_transform_and_no_color() {
        // Write two cubes via the kernel's own 3mf writer, read them back.
        let a = crate::kernel::Solid::cube(10.0, 10.0, 10.0, false);
        let b = crate::kernel::Solid::cube(5.0, 5.0, 5.0, false)
            .translate(fab_lang::Vec3::new(20.0, 0.0, 0.0));
        let path = std::env::temp_dir().join(format!("threemf_in_{}.3mf", std::process::id()));
        crate::kernel::Solid::write_3mf(&path, &[a, b]).unwrap();
        let objs = parse_3mf(&std::fs::read(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].tris.len(), 12);
        // The second cube sits at x ∈ [20, 25].
        let xs: Vec<f64> = objs[1].verts.iter().map(|v| v[0]).collect();
        assert!(xs.iter().cloned().fold(f64::MAX, f64::min) >= 19.9);
        // And both weld into valid solids through from_indexed (lift the 3mf arrays to Vec3/Tri).
        for o in &objs {
            let verts: Vec<fab_lang::Vec3> = o
                .verts
                .iter()
                .map(|&v| fab_lang::Vec3::from_array(v))
                .collect();
            let tris: Vec<fab_lang::Tri> = o.tris.iter().map(|&t| fab_lang::Tri(t)).collect();
            crate::kernel::Solid::from_indexed(&verts, &tris)
                .unwrap()
                .check()
                .unwrap();
        }
    }
}
