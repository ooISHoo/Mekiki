# Windows Input Reliability

Windows input injection is asynchronous from the target application's message
processing. A successful `SendInput` call does not prove that an application
accepted, interpreted, or persisted the input.

## Required scope

Keyboard and scroll operations require a named window scope. The literal
`focused` is available only for a workflow that established focus immediately
before the operation. A default visual search scope is not a safe default input
destination.

Before input is sent, Mekiki activates the named window and rejects an owner
window blocked by its modal dialog. Redirecting input silently to the dialog was
rejected because the script intended to address the owner.

## Text pacing

The default is one character per send with a 30 ms interval. The interval is
applied before the first character as well as between later sends. This matters
after Enter or a modified shortcut, where the receiving application may still
be processing the preceding key transition.

Line endings in `type_text` are translated to Enter presses and tabs to Tab
presses. Injecting those control characters as Unicode text did not produce the
expected editor actions.

Windows 11 Notepad has still dropped characters, and in one case a run of line
breaks, under paced multiline input. Other tested applications accepted the same
sequence. Treat tool success as delivery, not content verification; read the
field or resulting file when correctness matters.

## Modified keys

Modifier press, primary key press/release, and modifier release form one logical
operation. Cleanup must release every key even after interruption or error.
Operation boundaries must not allow following text to overtake a shortcut still
being processed by the target.

## Clipboard mode

A paste-based mode remains intentionally unimplemented. A correct design would
need explicit opt-in, preservation of all clipboard formats, protection against
temporarily exposing secrets, asynchronous paste-completion handling, and a
defined restoration policy for every failure point. Saving and restoring plain
text alone is not sufficient.

## Revisit criteria

Reopen the pacing decision only with a deterministic receiver or trace that can
separate injection order from application processing. Keep a reproducible script
and compare persisted output rather than relying on OCR of the editor surface.

