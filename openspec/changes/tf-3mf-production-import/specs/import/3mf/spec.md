# Spec Delta

## Purpose

How fab-scad turns a `.3mf` package into geometry, for OpenSCAD's `import()` and for opening a `.3mf` in the GUI: which build items count, how components and transforms place their meshes, and how an unreadable package fails.

## ADDED Requirements

### Requirement: Production-extension packages import
The system SHALL import a 3MF package whose root model reaches its meshes through components that name other model files by path (the 3MF Production extension, which Bambu Studio and MakerWorld use for every file). Model metadata SHALL NOT affect the import, wherever in the model it appears. DEFLATE-compressed and stored package entries SHALL both be read.

#### Scenario: Component in a sub-model file
- **WHEN** a `.scad` imports a 3MF whose root build item references an object made of one component that names `/3D/Objects/object_1.model` and an object id in that file
- **THEN** the imported geometry is that file's mesh, every triangle present, placed by the build item's and the component's transforms

#### Scenario: Metadata on both sides of the build
- **WHEN** the root model carries `<metadata>` elements before `<resources>` and again after `</build>`
- **THEN** the import succeeds and yields the same geometry as the same package without metadata

#### Scenario: Compressed package
- **WHEN** the package's entries are DEFLATE-compressed
- **THEN** the import succeeds as it does for an uncompressed package

### Requirement: Only the root model's build instantiates
The system SHALL instantiate exactly the build items of the package's root model, the part the package relationships name as the 3D model start part, falling back to `/3D/3dmodel.model` when no relationship names one. Objects in any other model file SHALL contribute geometry only through a component that references them, and build items declared in those files SHALL be ignored.

#### Scenario: Sub-model declares its own build items
- **WHEN** a sub-model file contains a `<build>` with items
- **THEN** those items add no geometry; only the root model's build items do

#### Scenario: Root model at a non-default path
- **WHEN** the package relationships name `/3D/main.model` as the start part and no `/3D/3dmodel.model` exists
- **THEN** `/3D/main.model`'s build items are the ones imported

### Requirement: Component placement follows the 3MF specification
The system SHALL place a component's mesh by applying the component's own transform first, then each enclosing component's, then the build item's. An absent transform and an explicit identity transform SHALL both leave the geometry where it is. This intentionally differs from the OpenSCAD oracle (observed in 2026.06.12), which raises a component without a non-identity transform by 1 mm in Z and applies a component's transform after its parent's. A differential mismatch on a component-bearing 3MF is this known divergence, not a regression.

#### Scenario: Component without a transform
- **WHEN** a unit cube spanning [0,1] on every axis is imported through a component with no transform under a build item with no transform
- **THEN** it spans z [0,1] (OpenSCAD 2026.06.12: z [1,2])

#### Scenario: Component with an explicit identity transform
- **WHEN** the same cube is imported through a component whose transform is `1 0 0 0 1 0 0 0 1 0 0 0`
- **THEN** it spans z [0,1] (OpenSCAD 2026.06.12: z [1,2])

#### Scenario: Translated component
- **WHEN** the same cube is imported through a component translated by 0.5 in Z
- **THEN** it spans z [0.5,1.5], which agrees with OpenSCAD

#### Scenario: Rotated build item over a translated component
- **WHEN** the same cube is imported through a component translated by 5 in Z, under a build item whose transform is `1 0 0 0 0 1 0 -1 0 0 0 0` (a quarter turn about X, carrying +Z to -Y)
- **THEN** it spans x [0,1], y [-6,-5], z [0,1] (OpenSCAD 2026.06.12: y [-1,0], z [5,6])

### Requirement: Unresolvable references fail the import and name what is missing
The system SHALL fail a 3MF import whose component names a model path the package lacks, whose component or build item names an object id its model file lacks, whose triangle references a vertex index past the mesh's vertex count, or whose components reference each other in a cycle. The failure message SHALL name the missing path, object id or index. From `import()` the failure SHALL surface as OpenSCAD's own warning, "Can't open import file '<name>': <reason>", and the model SHALL render without that import, as for any unreadable import.

#### Scenario: Component names a missing model file
- **WHEN** a component's path names `/3D/Objects/missing.model` and the package has no such part
- **THEN** `import()` warns "Can't open import file" with a reason naming `/3D/Objects/missing.model`, and the rest of the model renders

#### Scenario: Component names a missing object
- **WHEN** a component names object id 7 in a model file that has no object 7
- **THEN** the import fails with a reason naming object 7 and that model file

#### Scenario: Triangle index out of range
- **WHEN** a mesh with 4 vertices has a triangle referencing vertex 9
- **THEN** the import fails with a reason naming the out-of-range index instead of producing geometry

#### Scenario: Component cycle
- **WHEN** object 1's component references object 2 and object 2's component references object 1
- **THEN** the import fails promptly with an error, without hanging

### Requirement: Object colors and namespace prefixes
When the GUI opens a `.3mf`, an object that references a base-material group SHALL take that entry's display color, with the group resolved in the model file that declares the object. Per-triangle property overrides SHALL be ignored. Element and attribute namespace prefixes SHALL NOT matter: a materials table is read whatever prefix the file binds to the materials namespace. `import()` SHALL produce uncolored geometry, as OpenSCAD does.

#### Scenario: Object color from a prefixed materials table
- **WHEN** the GUI opens a 3MF whose object references entry 1 of an `<m:basematerials>` group whose entries are red then blue
- **THEN** the object displays blue

### Requirement: The model unit does not scale geometry
The system SHALL read 3MF coordinates as millimeters whatever the model's `unit` attribute says, matching the OpenSCAD oracle (which ignores it).

#### Scenario: Inch-unit model
- **WHEN** a model declaring `unit="inch"` contains a vertex at x=1
- **THEN** the imported vertex is at x=1, not 25.4

### Requirement: fab's own 3MF exports re-import unchanged
A 3MF written by fab's export SHALL import back with the same objects, triangle counts and placement.

#### Scenario: Two exported cubes
- **WHEN** fab exports a 10 mm cube at the origin and a 5 mm cube translated to x=20, then imports the file
- **THEN** two objects of 12 triangles each come back, the second spanning x [20,25]
