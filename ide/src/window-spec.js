const SHARED_HOSTS = new Set(["applicationframehost.exe"]);

/// Build a reusable selector and report ambiguity in the current snapshot.
export function windowSelectionFor(window, windows) {
  const exe = (window.exe || "").toLowerCase();
  const useExe = exe && !SHARED_HOSTS.has(exe);
  const parts = [];
  let matches = windows;
  if (useExe) {
    parts.push(`exe=${windowSpecValue(window.exe)}`);
    matches = matches.filter((w) => (w.exe || "").toLowerCase() === exe);
  }
  if (!useExe || matches.length > 1) {
    parts.push(`title_exact=${windowSpecValue(window.title)}`);
    matches = matches.filter((w) => w.title === window.title);
  }
  // WindowQuery matches class prefixes, not exact class names.
  if (matches.length > 1 && window.class_name) {
    const narrowed = matches.filter((w) =>
      (w.class_name || "").startsWith(window.class_name));
    if (narrowed.length < matches.length) {
      parts.push(`class=${windowSpecValue(window.class_name)}`);
      matches = narrowed;
    }
  }
  return {
    snippet: `window(${rhaiString(parts.join(","))})`,
    ambiguous: matches.length > 1,
  };
}

export function windowSpecFor(window, windows) {
  return windowSelectionFor(window, windows).snippet;
}

/// Escape delimiters understood by the structured window-spec parser.
export function windowSpecValue(value) {
  return value.replace(/\\/g, "\\\\").replace(/,/g, "\\,");
}

/// Turn a value into a Rhai string literal.
export function rhaiString(value) {
  return `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}
