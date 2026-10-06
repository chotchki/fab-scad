# Spec Delta

## Purpose

What the fab-scad GUI (desktop app and web editor) shows about the document that is open, when that document counts as unsaved, and what Save, Save As and Open do and report for each kind of document: a `.scadproj` archive, a loose `.scad` folder, a document with no file yet, and a hotchkiss.io item.

## ADDED Requirements

### Requirement: Unsaved state covers the whole document
The GUI SHALL treat the open document as unsaved from any change that Save would write until a save that writes it succeeds. This covers:
- editing the text of any file in the document;
- adding a text or binary file;
- creating, deleting or renaming a file;
- choosing a different entry file;
- changing cuts, connectors, a manual piece orientation or the printer bed size.

Viewing a different file SHALL NOT change whether the document is unsaved. In a loose folder, a change the folder already holds SHALL NOT make the document unsaved (see "Loose folders change on disk only through explicit file operations").

#### Scenario: Adding a file to an open .scadproj
- **WHEN** a `.scadproj` is open, the document is saved, and the user adds `hook.scad` from the Project tab
- **THEN** the document is unsaved: Save is enabled and ⌘S saves, and the archive that Save writes contains `hook.scad`

#### Scenario: Adding a binary asset
- **WHEN** a `.scadproj` is open and the user adds `logo.png`
- **THEN** the document is unsaved and the `logo.png` row shows the unsaved marker

#### Scenario: Viewing another file does not hide unsaved changes
- **WHEN** the user edits `main.scad` and then clicks the unedited `lib.scad` in the Project tab
- **THEN** the document is still unsaved, Save stays enabled, and the window title still shows "(unsaved)"

#### Scenario: Deleting a file or changing the entry in a .scadproj
- **WHEN** a saved `.scadproj` is open and the user deletes a non-entry file, or makes a different file the entry
- **THEN** the document is unsaved, and after Save the archive no longer contains the deleted file and reopens with the new entry rendering

#### Scenario: A cut plan edit
- **WHEN** the user moves a cut, places a connector, sets a piece orientation by hand, or changes the bed size
- **THEN** the document is unsaved

#### Scenario: An automatic plan on a model with no saved plan
- **WHEN** a model with no saved cut plan opens and the automatic plan computes for it
- **THEN** the document is not unsaved

#### Scenario: Resetting a saved plan to automatic
- **WHEN** a part has a saved cut plan and the user resets it to the automatic plan
- **THEN** the document is unsaved

### Requirement: Save is enabled exactly when there is something to save
The Save control SHALL be enabled when the document is unsaved and disabled when it is not. ⌘S (Ctrl+S on Windows and the web) SHALL save an unsaved document from any tab. On a document with nothing to save, ⌘S SHALL report that in the status line instead of doing nothing silently.

#### Scenario: ⌘S on a clean document
- **WHEN** the document `bracket.scadproj` has no unsaved changes and the user presses ⌘S
- **THEN** nothing is written and the status reads "nothing to save — bracket.scadproj is up to date"

#### Scenario: The Project tab offers Save for a .scadproj
- **WHEN** an unsaved `.scadproj` is open and the Project tab is showing
- **THEN** an enabled Save control is visible on the Project tab

### Requirement: Save reports its outcome and never half-writes an archive
Every save SHALL report its outcome in the status line. On success the status SHALL name the file or folder written. On failure the status SHALL say that the save failed and why, and every change that was not written SHALL remain unsaved. Writing a `.scadproj` SHALL replace the previous archive only once the new one is complete.

#### Scenario: Successful save
- **WHEN** the user saves `~/models/bracket.scadproj`
- **THEN** the status names `bracket.scadproj` and its folder, the document is no longer unsaved, and reopening the file shows exactly what was on screen

#### Scenario: A failed write
- **WHEN** the target file or folder cannot be written
- **THEN** the status says the save failed with the reason, the document stays unsaved, and the unsaved markers remain on the files that were not written

#### Scenario: An interrupted archive write
- **WHEN** a `.scadproj` save fails partway through writing
- **THEN** the previous `.scadproj` on disk is intact

### Requirement: Save writes the document where it lives
Save SHALL write according to the kind of document:
- **A `.scadproj`:** Save rewrites that archive with every file, every binary asset, the entry and the archive's title. The entry file carries the current cut plan and printer.
- **A loose `.scad` folder:** Save writes the entry file, with the current cut plan and printer, and every other file with unsaved edits, back into that folder.
- **A desktop document with no file yet:** Save asks where to save, as Save As does.
- **On the web:** Save downloads the document (a `.scad` for one file, a `.scadproj` for more), and a successful download counts as saved.

#### Scenario: The archive keeps its title
- **WHEN** a `.scadproj` whose manifest has a title is opened and saved
- **THEN** the saved archive's manifest has the same title

#### Scenario: Save on a desktop document with no file
- **WHEN** the desktop app was started without a file, the user has typed a model, and presses Save
- **THEN** a save dialog asks where to save it, and after a successful save the window title names the new file

### Requirement: Every desktop document can be saved under a new name
The desktop app SHALL offer Save As… on the Project tab and on ⇧⌘S (Ctrl+Shift+S) for every document. Save As SHALL write a `.scad` when the document is one text file with no assets, counting importable files that sit beside a loose file in its folder, and a `.scadproj` otherwise, so an `import()` is never stranded. Its dialog SHALL start in the document's own folder, or the user's home folder for a document with no file, never a temporary folder, and SHALL suggest the document's name. After a successful Save As, the document SHALL be the new file: later Saves write there, the title names it, and the original file is unchanged.

#### Scenario: Save As on an existing .scadproj
- **WHEN** `~/models/bracket.scadproj` is open and the user saves it as `~/models/bracket-v2.scadproj`
- **THEN** `bracket-v2.scadproj` holds the document, `bracket.scadproj` is unchanged, the status says the original was left unchanged, and the next Save writes `bracket-v2.scadproj`

#### Scenario: Packing a loose multi-file folder
- **WHEN** a loose folder with `main.scad` and `lib.scad` is open and the user chooses Save As… and a `.scadproj` name
- **THEN** the archive contains both files and the folder's assets, and the document is now that `.scadproj`

#### Scenario: Save As dialog location for an opened .scadproj
- **WHEN** a `.scadproj` from `~/models` is open and the user chooses Save As…
- **THEN** the dialog opens in `~/models`, suggesting the document's name

### Requirement: Opening a document replaces the open one
Opening a document SHALL show that document's saved contents and render its entry. Nothing in it may come from the previously open document's editor or cut plan, including when the two documents share a file or entry name. While the open document is unsaved, Open… SHALL require a press-and-hold to confirm discarding the unsaved changes; it SHALL NOT raise a modal dialog. After an open, the status SHALL name the document and its folder, never a temporary path. Passing a `.scadproj` path to the desktop app at launch SHALL open it as a project.

#### Scenario: Opening another archive with the same name
- **WHEN** `a/brace.scadproj` is open and the user opens `b/brace.scadproj`, whose entry file has the same name
- **THEN** the editor shows `b/brace.scadproj`'s entry text, the viewport re-renders it, its own saved cut plan applies, and the document is not unsaved

#### Scenario: Reopening the same file discards unsaved edits
- **WHEN** the user has unsaved edits to `bracket.scadproj`, holds Open… to confirm, and opens `bracket.scadproj` again
- **THEN** the editor shows the saved text and the document is not unsaved

#### Scenario: Open while unsaved needs a hold
- **WHEN** the document is unsaved and the user clicks Open… without holding
- **THEN** no file dialog opens and the open document is untouched

#### Scenario: Launching with a .scadproj
- **WHEN** the desktop app is launched as `fab-gui ~/models/bracket.scadproj`
- **THEN** the project opens with its entry rendering and the window title names `bracket.scadproj`

#### Scenario: Launching with a .scad that doesn't exist yet
- **WHEN** the desktop app is launched with the path of a `.scad` that does not exist
- **THEN** it opens an empty file named for that path, and the status says Save will write it there

#### Scenario: Undo never reaches into the previous document
- **WHEN** the user opens a document and immediately presses ⌘Z in the editor
- **THEN** the editor keeps the newly opened text; no text from the previously open document comes back

### Requirement: The open document is named on every screen
The GUI SHALL name the open document wherever the user is:
- **Window title (desktop):** `<document> — fab-scad`, with "(unsaved)" while unsaved, or `untitled — fab-scad` for a document with no file.
- **Header:** names the document on every tab.
- **Project tab:** opens with the document's name, its folder or origin, its kind (`.scadproj`, loose folder, a single `.scad` file, not saved yet, a hotchkiss.io item, or a web model with no site item), one line saying what Save does for it, and its Save, Save As… and Open… controls.
- **Model tab:** names the file in the editor and the document it belongs to, and, when that file is not the entry, which file renders.

User-facing text SHALL describe what Save does, not the archive format.

#### Scenario: Window title for an unsaved .scadproj
- **WHEN** `bracket.scadproj` is open with unsaved changes
- **THEN** the desktop window title is `bracket.scadproj (unsaved) — fab-scad`

#### Scenario: Viewing a library file
- **WHEN** the user views `hook.scad` in `bracket.scadproj`, whose entry is `main.scad`
- **THEN** the Model tab reads that `hook.scad` is in `bracket.scadproj` and `main.scad` renders

#### Scenario: Project tab for a .scadproj
- **WHEN** `~/models/bracket.scadproj` is open
- **THEN** the Project tab shows `bracket.scadproj`, `~/models`, that it is a `.scadproj`, and that Save rewrites it

### Requirement: Unsaved and stale look different
The marker for unsaved changes SHALL be visually distinct from the marker for a pipeline stage whose output is behind its input. Binary asset rows SHALL show the unsaved marker when they are unsaved.

#### Scenario: Both markers on screen
- **WHEN** a file row is unsaved and a tab is stale at the same time
- **THEN** the two markers differ in shape

### Requirement: Loose folders change on disk only through explicit file operations
For a document opened as a loose `.scad` folder:
- **Add** SHALL copy the added files into the folder immediately, `.scad` files included.
- **Rename** SHALL rename the file in the folder immediately.
- **Delete** SHALL remove the file from the session only, leave it in the folder, and say so wherever it is offered.
- **Text edits** SHALL reach the folder only through Save.

An Add or Rename that would overwrite an existing file in the folder SHALL be refused with a status message.

#### Scenario: Adding a file to a loose folder
- **WHEN** a loose folder is open and the user adds `hook.scad` from elsewhere
- **THEN** `hook.scad` is in the folder at once, and the document is not unsaved because of the add

#### Scenario: Deleting from a loose folder
- **WHEN** the user deletes `old.scad` from a loose folder's Project tab
- **THEN** `old.scad` leaves the list, stays in the folder on disk, and the control described it as removing from the session

### Requirement: The web saves what is on screen
On the web, "Save to hotchkiss.io" and Publish SHALL upload the current text of every file, including unsaved edits to the file in the editor. A successful site save SHALL clear the unsaved state; a failed one SHALL leave it.

#### Scenario: Site save of a multi-file item with an edit in progress
- **WHEN** a multi-file hotchkiss.io item is open, the user edits the file in the editor, and chooses "Save to hotchkiss.io" without switching files
- **THEN** the uploaded `.scadproj` contains the edit, matching the uploaded mesh, and the document is no longer unsaved once the upload succeeds
