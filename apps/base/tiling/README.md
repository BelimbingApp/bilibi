# bilibi-tiling

The tiled workspace at `/workspace`: several pages
side by side in one browser tab, arranged by a Hyprland-style dwindle
tree, with keyboard control, drag resizing, drag-to-swap, monocle and
layouts pinned per account or published for a company. `src/layout.rs` is the
pure tree and its compact URL form; `src/web.rs` is the page, which holds only
the tree, the focused tile, monocle, the names the tiles report and shared
layouts; `src/layouts.rs` keeps published layouts; `src/shared_web.rs` is the
shared-workspaces screen. This module owns no tables.

## Tiles are pages

A tile is a same-origin `<iframe>` of an existing page at its own URL. The
page runs as its own live session: its own socket, mount, URL state, flash
and access check, exactly as when opened alone, so twenty tiles cost what
twenty tabs cost. Web marks the frame's requests framed (`Sec-Fetch-Dest`),
Base UI renders them chromeless (`templates/framed.html`), and nothing in any
module changes to be shown in a tile. A tile whose page the account may not
open shows the refusal instead of a frame.

- **Frames survive.** Tiles render in creation order, never tree order, and
  are positioned by `data-place` alone, so a split, swap, move, resize, close
  or monocle never moves or re-creates a frame; the live patcher never rewrites
  an existing frame's `src`.
- **One scrollbar.** Inside a tile a page scrolls in the framed `#app-content`
  only, and a list page's table takes the tile's height under a sticky heading
  ("List fill" in `apps/base/ui/assets/css/app.css`). The workspace page itself
  never scrolls.
- **Back leaves the workspace.** A framed page never adds a history entry:
  `apps/base/ui/assets/live.js` replaces its live address changes and turns a
  plain link click into `location.replace`. The Tiling hook reports where each
  frame went (`tile_navigated`), and the workspace records the path in `?t=`
  with a replaced address. The workspace's own changes push, so Back steps
  through them and then leaves; within the workspace Back arrives as a params
  update (`data-history="params"`) and the tree read back is reconciled with
  the one on screen, so frames that stay keep their documents.
- **Narrow screens** (below `lg`) show the focused tile alone with a tab strip
  above to switch; the tree is kept, so widening restores the tiling. The
  sidebar's tile control opens the page normally there (`assets/shell.js`).

## Pinned and shared layouts

Pin the current tiles with **Pin this layout** in the top bar. Give the pin a name; the default joins the tile titles. The pin stores the workspace address, which holds its tree, and opens that snapshot from the top bar. Unpin it from the top bar like any other page.

The shared-workspaces control lists layouts published for the current workplace. `/workspace/shared-layouts` (System › Shared workspaces) lets an authorized person publish the current tree for the company or selected roles. Each tile checks the viewer's access to its page. Shared layouts live only in `ui.workspace.shared_layouts` at company scope through Settings; an account without a current desk cannot read or publish one.

## Keys

`apps/base/ui/assets/tiling.js` implements each key and this table is their
documentation; change them together. The listener is attached to every
frame's document too, so the keys work wherever focus is.

| Key | Does |
| --- | --- |
| `Ctrl+.` | Enter or leave tiling mode (shown in the status bar) |
| `h` `j` `k` `l`, arrows | Focus the tile in that direction |
| `H` `J` `K` `L` | Move the focused tile that way |
| `s` | Swap the focused tile with its neighbour in the last direction |
| `r`, then arrows | Resize; `Esc` returns to tiling mode |
| `f` | Fill the workspace with the focused tile, or show every tile again |
| `t` | Flip the split direction |
| `w` | Follow the records other tiles select, or stop following |
| `o` | Open the focused tile's page alone |
| `n` | Add a page |
| `q` | Close the focused tile |
| `Esc` | Leave tiling mode (an open menu takes it first) |


With a pointer: drag a divider to resize (a focused divider also moves with
its arrow keys), drag a tile's Move control onto another tile to swap them, and
use the tile's Flip and Close controls. Clicking Move enters tiling mode, where
`H` `J` `K` `L` move the tile. Pressing anywhere in a tile, inside its page
included, focuses it.

## Following selections

Tiles talk through the follow channel (`apps/base/ui/src/workspace.rs`). A
tile showing one record's page follows selections after `w`: the tree keeps
that page's route pattern (`/companies/42>/companies/:id` in the URL form),
the tile shows a link mark, and the tile carries `data-follow="on"`
(`"off"` on a tile that could follow). When a page in any tile announces a
record of the module that owns the pattern, the workspace fills the id in,
records the new path with a replaced address, and the hook sends the frame
there (`tile_navigate`). `w` on a page that names no single record answers
with a notice.

The channel never leaves the tab: the frames are same-origin and talk to
the workspace with `postMessage`, and each side accepts only this origin and,
on the workspace's side, its own frames; the server checks every fact's kind
and id again, and the hook sends a frame only to a path of this origin. The
workspace renders the kinds some tile follows as `data-follows`, and the hook
tells each frame. A page opts in with Base UI's `lists::record_link` on a
list row (`workspace` = the page is framed: a `workspace:select` button that
the frame answers, selecting when its kind is followed and otherwise opening
the record in the tile) and `workspace::announce` on a record's page. The
Company list and record pages do.

## Try it

Run the app (`./bilibi start`, or the manual commands in the root
README), sign in, and use a sidebar row's "Open in a tile" control, or open
`/workspace` and add pages. Then: navigate inside a tile and reload (the tile
keeps its page); press `Ctrl+.` and use the keys above; drag a divider and a
tile's Move control; press Back (it undoes the last workspace change, then leaves); narrow the
window below 1024px (one tile and a tab strip). With the workspace open, a
sidebar tile control adds a tile and the tiles on screen keep their pages.

To follow: open `/workspace?t=h.5(/companies,/companies/1)`, press `Ctrl+.`,
focus the record tile and press `w`, then click another company's name in the
list tile: the record tile goes there and the list stays put. With nothing
following, the same click opens the company in the list's own tile.

Pin a three-tile layout, give it a name, and open it from the top bar. Remove it with the pin's unpin action.

To share a layout, use the shared-workspaces control and choose “Share current tiles”.
