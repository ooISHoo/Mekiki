// Basic, read-only inspection. This script does not move the mouse or type.
//
//   target\release\mekiki.exe run examples\01-observe.rhai

print("=== visible windows ===");
let titles = window_titles();
print(`${titles.len()} window(s)`);
for i in 0..min(titles.len(), 5) {
    print(`  ${titles[i]}`);
}

print("");
print("=== primary screen ===");
let s = screen();
print(`${s.width}x${s.height} at (${s.x}, ${s.y})`);

let width = min(s.width, 400);
let height = min(s.height, 300);
let center = region(
    s.x + (s.width - width) / 2,
    s.y + (s.height - height) / 2,
    width,
    height,
);
print(`center: ${center}`);
print(`below:  ${center.below(100)}`);
print(`grown:  ${center.grow(10)}`);

print("");
print("=== coordinate locators ===");
let point = target(`point:${center.x},${center.y}`).resolve();
print(`point:  ${point}`);
let area = target(
    `region:${center.x},${center.y},${center.width},${center.height}`,
).resolve();
print(`region center: (${area.center_x}, ${area.center_y})`);

set_similarity(0.85);
set_timeout(5000);
print("settings changed for this run only");

fn min(a, b) {
    if a < b { a } else { b }
}
