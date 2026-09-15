// English catalog.
//
// Keep labels concise, tooltips action-oriented, and status or log messages
// explicit about the result. Placeholders use the shared `$1`, `$2` syntax.

export default {
  ui: {
    // --- Toolbar ---
    "toolbar.new.hint": "New Script (Ctrl+N)",
    "toolbar.open.hint": "Open Script (Ctrl+O)",
    "toolbar.save.hint": "Save (Ctrl+S)",
    "toolbar.saveAs.hint": "Save As (Ctrl+Shift+S)",
    "toolbar.snip.hint": "Open Snipping Tool",
    "toolbar.paste.hint": "Save Clipboard Image",
    "toolbar.run.hint": "Run Script (Ctrl+Enter)",
    "toolbar.pause.hint": "Pause",
    "toolbar.resume.hint": "Resume",
    "toolbar.stop.hint": "Stop (Shift+Alt+C)",
    "toolbar.stepMode.hint": "Step Mode",
    "toolbar.stepNext.hint": "Next Statement (F10)",
    "toolbar.preview": "Preview Match",
    "toolbar.preview.hint":
      "Show where the selected image matches on the desktop (Ctrl+P)",
    "toolbar.baseDir": "Working Directory",
    "toolbar.baseDir.hint": "Working Directory",
    "toolbar.settings.hint": "Settings",

    // --- Settings ---
    "settings.title": "Settings",
    "settings.language": "Display language",
    "settings.language.system": "System default",
    "settings.theme": "Appearance",
    "settings.theme.system": "System default",
    "settings.theme.light": "Light",
    "settings.theme.dark": "Dark",
    "settings.editorFont": "Editor font size",
    "settings.apply": "Apply",
    "settings.close": "Close",
    "settings.applied": "Settings applied",

    // --- API completion ---
    "apiInfo.global": "Global",
    "apiInfo.parameters": "Parameters",
    "apiInfo.constraints": "Constraints",
    "apiInfo.errors": "Errors",
    "apiInfo.examples": "Examples",

    // --- Bottom panel tabs ---
    "tab.output": "Output",
    "tab.windows": "Windows",

    // --- Right pane: image assets ---
    "assets.title": "Image Assets",
    "assets.refresh.hint": "Refresh Folder",
    "assets.tileSize.hint": "Thumbnail size",
    "assets.tileSize.smaller.hint": "Decrease thumbnail size",
    "assets.tileSize.larger.hint": "Increase thumbnail size",
    "assets.empty": "No images",
    "assets.loading": "Loading...",
    "assets.failed": "Could not load image assets: $1",
    "assets.count": "Images: $1",
    "assets.inserted": "Inserted $1",
    "assets.storeBadge": "Temporary",
    "assets.rename": "Rename",
    "assets.rename.hint": "Click the file name or press F2",
    "assets.renamePrompt": "New file name",
    "assets.renamed": "Renamed $1 to $2",
    "assets.renamedWithRefs":
      "Renamed $1 to $2. Updated $3 references in the script",
    "assets.namedFromStore": "Saved the temporary image as $1",
    "assets.namedFromStoreWithRefs":
      "Saved the temporary image as $1. Updated $2 references in the script",
    "assets.renameNone": "No image is available to rename",
    "assets.menuInsert": "Insert into Script",
    "assets.delete": "Delete Image",
    "assets.deleteConfirm": "Delete $1?",
    "assets.deleted": "Deleted $1",
    "assets.deletedWithRefs":
      "Deleted $1. References still in the script: $2",

    // --- Match preview ---
    "preview.similarity": "Similarity",
    "preview.hint": "Select an image tile, then click Preview Match",
    "preview.noMatch": "$1: No match (minimum similarity: $2)",
    "preview.matched": "$1: Matches: $2; best score: $3",
    "preview.matchLine": "  ($1, $2) $3x$4  $5",
    "preview.failed": "Match preview failed: $1",

    // --- Windows pane ---
    "windows.refresh": "Refresh",
    "windows.loading": "Loading...",
    "windows.inserted": "Inserted window locator",
    "windows.ambiguous": "This locator matches multiple windows. Refine it before running; otherwise the frontmost match will be selected.",
    "windows.menuInsert": "Insert at Cursor",
    "windows.menuLog": "Log Details",
    "windows.detailTitle": "Title: $1",
    "windows.detailExe": "Executable: $1",
    "windows.detailBounds": "Position: ($1, $2)  Size: $3x$4",
    "windows.detailClass": "Class: $1",
    "windows.detailZ": "Z-order: $1",

    // --- File operations ---
    "file.untitled": "(Untitled)",
    "file.new": "New script",
    "file.unsaved.title": "Save Changes?",
    "file.unsaved.message": "Save changes to $1?",
    "file.unsaved.save": "Save",
    "file.unsaved.discard": "Don't Save",
    "file.unsaved.cancel": "Cancel",
    "file.openPrompt": "Path of the script to open",
    "file.openTitle": "Open Script",
    "file.opened": "Opened $1",
    "file.openFailed": "Could not open $1",
    "file.savePrompt": "Path to save the script",
    "file.saveAsTitle": "Save Script As",
    "file.rhaiFilter": "Rhai Scripts",
    "file.saved": "Saved to $1",
    "file.saveFailed": "Save failed: $1",

    // --- Image capture and import ---
    "image.snipHint": "After taking a snip, click Save Clipboard Image",
    "image.imported": "Imported: $1...",
    "image.needFolder.title": "Cannot Save Image",
    "image.needFolder.message":
      "Save the script to establish its working folder before importing images.",
    "image.detectFailed": "Could not scan image references: $1",
    "image.notFound": "$1 (not found)",

    // --- Script execution ---
    "run.running": "Running...",
    "run.runningWithHotkey": "Running... Press $1 to stop",
    "run.hotkeyUnavailable":
      "Could not register the emergency stop shortcut. Another application may be using it. The Stop button is still available: $1",
    "run.paused": "Paused. Resume to continue using the IDE",
    "run.stopping": "Stopping...",
    "run.stopped": "Stopped ($1 ms)",
    "run.stepping": "Stepping: line $1",
    "run.stepModeOn": "Step mode enabled",
    "run.stepModeOff": "Step mode disabled",
    "run.done": "Completed ($1 ms)",
    "run.failed": "Failed ($1 ms)",
    "run.unknownError": "Unknown error",
    "run.cannotStart": "Could not start script",

    // --- Window controls ---
    "titlebar.minimize.hint": "Minimize",
    "titlebar.maximize.hint": "Maximize",
    "titlebar.restore.hint": "Restore Down",
    "titlebar.close.hint": "Close",

    // --- Pane dividers (screen readers) ---
    "splitter.right":
      "Divider between the editor and right pane. Use the arrow keys to resize; press Home to reset",
    "splitter.bottom":
      "Divider between the upper and lower panes. Use the arrow keys to resize; press Home to reset",

    // --- Engine status ---
    "status.starting": "Starting...",
    "status.ready": "Ready",
    "status.engineVersion": "Mekiki Engine $1",
    "status.ideVersion": "Mekiki IDE $1",
    "status.cursor": "Line $1, Column $2",
    "status.cursor.hint": "Cursor position",
  },

  // CodeMirror uses English source strings as keys and falls back to those
  // strings when no translation is registered.
  codemirror: {},
};
