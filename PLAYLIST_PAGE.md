# Playlists Page Implementation

The Playlists Page provides a full-screen, grid-based view of the user's playlists with cover images (when the `image` feature is enabled).

## Architectural Overview

### 1. State Management
- **`PlaylistsPageUIState`**: Tracks the `selected_index` and a `rendered` boolean flag. The `rendered` flag is reset whenever the selection changes or the view is scrolled to trigger a re-render of the images.
- **`PlaylistsPageRenderInfo`**: Stored in the global `UIState`, it tracks the current view's metadata:
  - `render_areas`: A list of `Rect`s where images were last printed.
  - `rect`: The inner layout rectangle of the page.
  - `start_row`, `items_per_row` & `max_visible_rows`: Used to detect if the user has scrolled or resized the terminal. The page displays playlists in a row-major grid and uses page-based scrolling.

### 2. Image Rendering Pipeline
- **Sparse Loading**: If an image is not in the `images` cache (TTL-based, size 256), the UI sends a `ClientRequest::LoadImage` to the background client. Once loaded, the page marks itself as not `rendered` to trigger a reprint.
- **View Change Detection**: Each render frame, the current view metadata is compared against `PlaylistsPageRenderInfo`. If a change (scroll, resize, or navigation) is detected:
  1. The previous image areas are cleared using `utils::clear_area`.
  2. `render_areas` is emptied.
  3. `state.rendered` is set to `false`.
- **Scrolling**: The page uses page-based scrolling. When the selected row moves past the visible range, the `start_row` jumps by `max_visible_rows` to show the next page of rows.
- **Drawing**: Images are printed using `viuer::print`. To prevent flickering and redundant terminal I/O, an image is only printed if its `Rect` is not already present in the `render_areas` list.
- **Buffer Integrity**: For every rendered image, the underlying Ratatui buffer cells are marked with `set_skip(true)` to prevent the TUI from overwriting the image with text or background styles.

### 3. Navigation & Cleanup
- **Navigation**: Navigation is row-major. `SelectNextOrScrollDown` (`j`) moves to the same column in the next row, while `FocusNextWindow` (`Tab`) moves to the next item in the same row.
- **Cross-Page Cleanup**: When navigating away from the Playlists page, the main UI loop (`ui/mod.rs`) uses the `render_areas` stored in `UIState` to clear all terminal image artifacts, preventing them from overlapping with other pages.
