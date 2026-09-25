//! Cover images for the Playlists page grid.
//!
//! Every cover is encoded once per grid cell size, on background workers, into a
//! [`SlicedProtocol`]. Scrolling then only changes which rows of an already encoded image are
//! shown: with the Kitty protocol an image is sent to the terminal once, and later frames only
//! move its text placeholders.

use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU16,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};

use anyhow::Context as _;
use image::DynamicImage;
use ratatui::{
    buffer::{Buffer, CellDiffOption},
    layout::{Position, Rect, Size},
    widgets::Widget,
};
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::kitty::Kitty,
    sliced::{SignedPosition, SlicedImage, SlicedProtocol},
    Resize,
};

use crate::{
    client::ClientRequest,
    config::AppConfig,
    state::{AppData, State},
};

const ENCODE_WORKERS: usize = 2;
/// How long to wait for a requested cover image before asking the client for it again.
const IMAGE_REQUEST_RETRY: Duration = Duration::from_secs(10);
/// Approximate memory kept for encoded covers. Kitty protocols hold on to their transmit data.
const MEMORY_BUDGET: usize = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;
/// Cap on new Kitty image data sent per frame (at least one cover always goes through). A row of
/// unseen covers then arrives over a few frames instead of stalling a single one.
pub const TRANSMIT_BUDGET: usize = 2 * 1024 * 1024;

/// Geometry of the Playlists page grid, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridLayout {
    pub items_per_row: usize,
    pub item_width: u16,
    pub img_cols: u16,
    pub img_rows: u16,
    /// Cover, title and one blank row.
    pub item_height: u16,
    /// Offset that centres the grid horizontally.
    pub left_margin: u16,
}

impl GridLayout {
    pub fn new(inner: Rect, font_ratio: f32, config: &AppConfig) -> Self {
        let base_img_length = if config.cover_img_length > 0 {
            config.cover_img_length as u16
        } else {
            (config.cover_img_width as f32 / font_ratio).round() as u16
        };

        // increase items per row by sqrt(2) to approximately double the number of items per page
        let items_per_row = (f32::from((inner.width / (base_img_length + 2)).max(1)) * 1.414)
            .round()
            .max(1.0) as usize;
        let item_width = inner.width / items_per_row as u16;
        let img_cols = item_width.saturating_sub(2);
        let img_rows = (f32::from(img_cols) * font_ratio).round() as u16;
        let content_width = (items_per_row as u16).saturating_sub(1) * item_width + img_cols;

        Self {
            items_per_row,
            item_width,
            img_cols,
            img_rows,
            item_height: img_rows + 2,
            left_margin: inner.width.saturating_sub(content_width) / 2,
        }
    }
}

/// Everything an encoded cover depends on. Covers encoded for another key are unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellKey {
    size: Size,
    font_size: (u16, u16),
    protocol: ProtocolType,
}

struct CoverEntry {
    proto: SlicedProtocol,
    kitty_id: Option<u32>,
    bytes: usize,
    last_used: u64,
    /// Whether the image data has been sent to the terminal (Kitty sends it on first render).
    sent: bool,
}

struct Job {
    generation: u64,
    url: String,
    key: CellKey,
    picker: Picker,
    kitty_id: u32,
}

struct Encoded {
    generation: u64,
    url: String,
    kitty_id: u32,
    /// `None` if the source image left the image cache before the job ran.
    result: anyhow::Result<Option<(SlicedProtocol, usize)>>,
}

struct Encoder {
    jobs: flume::Sender<Job>,
    results: flume::Receiver<Encoded>,
    /// Lets workers skip jobs queued before the cell size last changed.
    generation: Arc<AtomicU64>,
}

impl Encoder {
    fn spawn(state: &Weak<State>, generation: u64) -> Self {
        let (jobs, job_rx) = flume::unbounded::<Job>();
        let (result_tx, results) = flume::unbounded();
        let generation = Arc::new(AtomicU64::new(generation));
        for _ in 0..ENCODE_WORKERS {
            let (job_rx, result_tx, state, generation) = (
                job_rx.clone(),
                result_tx.clone(),
                state.clone(),
                generation.clone(),
            );
            std::thread::spawn(move || {
                // exits once the store (and with it the job sender) is dropped
                while let Ok(job) = job_rx.recv() {
                    if job.generation != generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    // the weak reference keeps workers from holding the app state alive
                    let Some(state) = state.upgrade() else { break };
                    let image = state.data.read().caches.images.get(&job.url).cloned();
                    drop(state);
                    let result = image.map(|img| encode(&job, &img)).transpose();
                    let done = Encoded {
                        generation: job.generation,
                        url: job.url,
                        kitty_id: job.kitty_id,
                        result,
                    };
                    if result_tx.send(done).is_err() {
                        break;
                    }
                }
            });
        }
        Self {
            jobs,
            results,
            generation,
        }
    }
}

/// Stretch `img` to exactly fill the cover cells (so no padding shows) and encode it for the
/// picker's protocol. Returns the protocol and its approximate memory footprint.
fn encode(job: &Job, img: &DynamicImage) -> anyhow::Result<(SlicedProtocol, usize)> {
    let (font_w, font_h) = job.key.font_size;
    let size = job.key.size;
    let (px_w, px_h) = (
        u32::from(size.width) * u32::from(font_w),
        u32::from(size.height) * u32::from(font_h),
    );
    let stretched = img.resize_exact(px_w, px_h, image::imageops::FilterType::Triangle);
    let rgba_bytes = px_w as usize * px_h as usize * 4;

    if job.key.protocol == ProtocolType::Kitty {
        // built directly so the store knows the image id and can delete it from the terminal
        let kitty = Kitty::new(stretched, size, job.kitty_id, is_tmux())
            .context("encode kitty cover image")?;
        // the transmit sequence is base64 encoded RGBA
        Ok((SlicedProtocol::Kitty(kitty), rgba_bytes / 3 * 4))
    } else {
        let proto =
            SlicedProtocol::new_with_resize(&job.picker, stretched, size, Resize::Fit(None))
                .context("encode cover image")?;
        Ok((proto, rgba_bytes))
    }
}

/// Same check `ratatui-image` uses to decide whether to wrap escapes for tmux passthrough.
fn is_tmux() -> bool {
    std::env::var("TERM").is_ok_and(|term| term.starts_with("tmux"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|program| program == "tmux")
}

fn kitty_delete(id: u32) -> String {
    if is_tmux() {
        format!("\x1bPtmux;\x1b\x1b_Ga=d,d=I,i={id},q=2\x1b\x1b\\\x1b\\")
    } else {
        format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
    }
}

/// Encoded covers for the Playlists page, keyed by image URL.
#[derive(Default)]
pub struct CoverStore {
    key: Option<CellKey>,
    generation: u64,
    frame: u64,
    entries: HashMap<String, CoverEntry>,
    pending: HashSet<String>,
    failed: HashSet<String>,
    requested_images: HashMap<String, Instant>,
    /// Kitty image data sent during the current frame.
    frame_sent_bytes: usize,
    pending_deletes: Vec<u32>,
    next_kitty_id: u32,
    encoder: Option<Encoder>,
    /// Total number of covers encoded so far.
    pub encoded: usize,
}

impl std::fmt::Debug for CoverStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoverStore")
            .field("key", &self.key)
            .field("entries", &self.entries.len())
            .field("pending", &self.pending.len())
            .field("encoded", &self.encoded)
            .finish_non_exhaustive()
    }
}

impl CoverStore {
    /// Drop every encoded cover, e.g. when the terminal is resized.
    pub fn clear(&mut self) {
        self.generation += 1;
        if let Some(encoder) = &self.encoder {
            encoder.generation.store(self.generation, Ordering::Relaxed);
        }
        self.pending.clear();
        self.failed.clear();
        self.pending_deletes
            .extend(self.entries.drain().filter_map(|(_, e)| e.kitty_id));
    }

    /// Whether covers are still being encoded.
    #[cfg(test)]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Start a frame: switch to the grid's current cover size and collect finished encodes.
    pub fn begin_frame(&mut self, state: &Arc<State>, picker: &Picker, cover_size: Size) {
        self.frame += 1;
        self.frame_sent_bytes = 0;
        let font_size = picker.font_size();
        let key = CellKey {
            size: cover_size,
            font_size: (font_size.width.max(1), font_size.height.max(1)),
            protocol: picker.protocol_type(),
        };
        if self.key != Some(key) {
            self.clear();
            self.key = Some(key);
        }
        if self.encoder.is_none() {
            self.next_kitty_id = rand::random::<u32>() % 0x00f0_0000 + 1;
            self.encoder = Some(Encoder::spawn(&Arc::downgrade(state), self.generation));
        }

        let Some(encoder) = &self.encoder else { return };
        while let Ok(done) = encoder.results.try_recv() {
            if done.generation != self.generation {
                continue;
            }
            self.pending.remove(&done.url);
            match done.result {
                Ok(Some((proto, bytes))) => {
                    self.encoded += 1;
                    let kitty_id =
                        matches!(proto, SlicedProtocol::Kitty(_)).then_some(done.kitty_id);
                    self.entries.insert(
                        done.url,
                        CoverEntry {
                            proto,
                            kitty_id,
                            bytes,
                            last_used: self.frame,
                            sent: false,
                        },
                    );
                }
                // the source image was evicted meanwhile; `request` will load it again
                Ok(None) => {}
                Err(err) => {
                    tracing::error!("Failed to encode playlist cover {}: {err:#}", done.url);
                    self.failed.insert(done.url);
                }
            }
        }
    }

    /// Make sure the cover for `url` is (being) prepared. Request visible covers before
    /// prefetched ones: at most `max_pending` encodes are queued at a time.
    pub fn request(
        &mut self,
        data: &AppData,
        client: &flume::Sender<ClientRequest>,
        picker: &Picker,
        url: &str,
        max_pending: usize,
    ) {
        if let Some(entry) = self.entries.get_mut(url) {
            entry.last_used = self.frame;
            return;
        }
        if self.pending.contains(url) || self.failed.contains(url) {
            return;
        }
        if !data.caches.images.contains_key(url) {
            let due = self
                .requested_images
                .get(url)
                .is_none_or(|at| at.elapsed() >= IMAGE_REQUEST_RETRY);
            if due {
                client
                    .send(ClientRequest::LoadImage(url.to_string()))
                    .unwrap_or_default();
                self.requested_images
                    .insert(url.to_string(), Instant::now());
            }
            return;
        }
        if self.pending.len() >= max_pending {
            return;
        }
        let (Some(key), Some(encoder)) = (self.key, &self.encoder) else {
            return;
        };
        let job = Job {
            generation: self.generation,
            url: url.to_string(),
            key,
            picker: picker.clone(),
            kitty_id: self.next_kitty_id,
        };
        self.next_kitty_id = self.next_kitty_id.wrapping_add(1).max(1);
        if encoder.jobs.send(job).is_ok() {
            self.pending.insert(url.to_string());
        }
    }

    /// Render the cover for `url` at `pos` (relative to `clip`, may be negative) into `clip`.
    /// Rows outside `clip` are skipped. Covers that aren't ready yet are left blank.
    pub fn render(&mut self, buf: &mut Buffer, url: &str, clip: Rect, pos: SignedPosition) {
        let Some(entry) = self.entries.get_mut(url) else {
            return;
        };
        entry.last_used = self.frame;

        let height = entry.proto.size().height;
        let on_screen =
            i32::from(pos.y) < i32::from(clip.height) && i32::from(pos.y) + i32::from(height) > 0;
        if entry.kitty_id.is_some() && !entry.sent && on_screen {
            if self.frame_sent_bytes > 0 && self.frame_sent_bytes + entry.bytes > TRANSMIT_BUDGET {
                return;
            }
            self.frame_sent_bytes += entry.bytes;
            entry.sent = true;
        }
        SlicedImage::new(&entry.proto, pos).render(clip, buf);
    }

    /// Finish a frame: evict covers not used recently (keeping at least `keep`) and tell the
    /// terminal to free evicted Kitty images, via the cell at `anchor`.
    pub fn end_frame(&mut self, buf: &mut Buffer, anchor: Position, keep: usize) {
        let limit = MAX_ENTRIES.max(keep);
        let mut total: usize = self.entries.values().map(|e| e.bytes).sum();
        while self.entries.len() > keep && (total > MEMORY_BUDGET || self.entries.len() > limit) {
            let Some(url) = self
                .entries
                .iter()
                .filter(|(_, e)| e.last_used < self.frame)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(url, _)| url.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&url) {
                total -= entry.bytes;
                self.pending_deletes.extend(entry.kitty_id);
            }
        }

        if self.pending_deletes.is_empty() {
            return;
        }
        if let Some(cell) = buf.cell_mut(anchor) {
            let mut symbol: String = self.pending_deletes.drain(..).map(kitty_delete).collect();
            symbol.push_str(cell.symbol());
            // the escapes take no space on screen; without this ratatui would compute a width
            // from their text and skip the cells after the anchor
            cell.set_symbol(&symbol)
                .set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
        }
    }
}
