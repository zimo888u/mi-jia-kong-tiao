# Local Windows repaint fixes

This is the published `i-slint-backend-winit` 1.18.0 crate, with its original
source, copyright notices and `LICENSES` directory preserved. Cargo uses this
workspace-local copy rather than modifying the user's registry cache.

One upstream fix is backported into `winitwindowadapter.rs`, with a local
`LogicalRect` import needed by 1.18.0; the fix's logic is unchanged:

- Slint PR: https://github.com/slint-ui/slint/pull/13568
- Merged commit: `08a297b7151e52b98b500b4424a8dee770fc7b58`
- Change: before drawing on Windows, read the native update rectangle and mark
  that region dirty. An ordinary Slint-requested internal paint still uses
  partial rendering; OS-invalidated regions are rendered and presented too.

This prevents the software surface's retained buffer age from suppressing
painting after exposure, offscreen movement, restoration, or DPI changes.

An additional local fix tracks Windows' observed minimized state separately
from Slint's requested state. On restore it explicitly marks the complete
client area dirty and requests a native redraw on the existing HWND. This
replaces the application's old hide/show recovery, which destroyed and
recreated the native window and caused a visible flash after restoration.
Normal device updates continue to use partial rendering.

The native offline regression is `crates/miac-app/examples/window-repaint.rs`.
It simulates device state changes with the window partly or fully offscreen,
invalidates its displayed pixels, then compares the returned window with a
freshly rendered reference. It touches only its own window, uses no account or
device I/O, and covers all five themes. Run it with:

```text
cargo run -p miac-app --example window-repaint --offline --locked -- target/window-repaint/check
```

The restore regression records native hide/destroy messages and HWND identity,
checks normal/maximized bounds, and compares early presented frames against a
complete reference across five themes and both native and Slint restore paths:

```text
cargo run -p miac-app --example window-repaint --release --offline --locked -- target/window-repaint/restore --restore --rounds=3
```

Remove this override when the project's Slint release includes this fix; do not
discard other pinned dependency versions merely to change repaint behavior.
