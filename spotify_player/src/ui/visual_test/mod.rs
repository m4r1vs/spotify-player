//! Visual regression tests for the Playlists page.
//!
//! Each scenario renders the page into a `TestBackend` and compares it against a golden file:
//! a PNG for halfblocks (the buffer holds the full picture) and a structural text snapshot for
//! Kitty (the buffer only holds placeholders). Every run writes `target/visual/*` for review;
//! `compare.png` shows golden | actual | diff side by side.
//!
//! Re-bless the goldens with `SPOTIFY_PLAYER_UPDATE_GOLDENS=1`.

mod raster;

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use image::{DynamicImage, Rgb, RgbImage};
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use ratatui_image::picker::{Picker, ProtocolType};
use rspotify::model::{PlaylistId, UserId};

use crate::{
    client::ClientRequest,
    state::{
        Mutex, PageState, Playlist, PlaylistFolderItem, PlaylistsPageUIState, PopupState,
        SharedState, State, TTL_CACHE_DURATION,
    },
};

const N_PLAYLISTS: usize = 40;
const GENRES: [&str; 5] = ["Rock", "Jazz", "Lo-Fi", "Techno", "Indie"];
/// Scenarios run at roughly the size of the user's Playlists page (terminal minus playback bar).
const WIDE: (u16, u16) = (253, 54);
const NARROW: (u16, u16) = (120, 40);

fn init_config() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        // Render as in a plain terminal. Inside tmux, `Picker` wraps escapes for passthrough and
        // runs `tmux set` on the caller's pane.
        std::env::remove_var("TMUX");
        std::env::set_var("TERM", "xterm-256color");
        std::env::set_var("TERM_PROGRAM", "ghostty");

        let dir =
            std::env::temp_dir().join(format!("spotify-player-visual-test-{}", std::process::id()));
        let config_dir = dir.join("config");
        let cache_dir = dir.join("cache");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(cache_dir.join("image")).unwrap();
        // mirrors the layout knobs of the config the page was tuned with
        std::fs::write(
            config_dir.join("app.toml"),
            "cover_img_length = 33\ncover_img_width = 15\nplaylist_page_default = true\n",
        )
        .unwrap();
        crate::config::set_config(crate::config::Configs::new(&config_dir, &cache_dir).unwrap());
    });
}

fn playlist_name(i: usize) -> String {
    if i == 13 {
        "A Very Long Playlist Name That Overflows The Cover".to_string()
    } else {
        format!("{} Mix {i:02}", GENRES[i % GENRES.len()])
    }
}

fn cover_url(i: usize) -> String {
    format!("https://covers.test/{i:02}.jpg")
}

fn playlist(i: usize) -> Playlist {
    Playlist {
        id: PlaylistId::from_id(format!("{i:0>22}")).unwrap(),
        collaborative: false,
        name: playlist_name(i),
        owner: ("tester".to_string(), UserId::from_id("tester").unwrap()),
        desc: String::new(),
        current_folder_id: 0,
        snapshot_id: String::new(),
        cover_url: Some(cover_url(i)),
    }
}

fn hsl(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r, g, b].map(|v| ((v + m) * 255.0).round() as u8)
}

/// A 640x640 cover: distinct hue per playlist, 16 horizontal bands getting lighter towards the
/// bottom (so vertical clipping is visible) and the index drawn large in the middle.
fn cover_image(i: usize) -> DynamicImage {
    use font8x8::UnicodeFonts;
    const SIZE: u32 = 640;
    const SCALE: u32 = 16;
    let hue = i as f32 * 360.0 / N_PLAYLISTS as f32;
    let mut img = RgbImage::new(SIZE, SIZE);
    for (_, y, px) in img.enumerate_pixels_mut() {
        let band = y / (SIZE / 16);
        let light = if band % 2 == 0 { 0.30 } else { 0.45 } + band as f32 * 0.02;
        *px = Rgb(hsl(hue, 0.7, light));
    }
    let text = format!("{i:02}");
    let x0 = (SIZE - text.len() as u32 * 8 * SCALE) / 2;
    let y0 = (SIZE - 8 * SCALE) / 2;
    for (n, ch) in text.chars().enumerate() {
        let glyph = font8x8::BASIC_FONTS.get(ch).unwrap();
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) == 0 {
                    continue;
                }
                let gx = x0 + (n as u32 * 8 + col) * SCALE;
                let gy = y0 + row as u32 * SCALE;
                for py in gy..gy + SCALE {
                    for px in gx..gx + SCALE {
                        img.put_pixel(px, py, Rgb([255, 255, 255]));
                    }
                }
            }
        }
    }
    DynamicImage::ImageRgb8(img)
}

struct Harness {
    state: SharedState,
    client_rx: flume::Receiver<ClientRequest>,
    terminal: Terminal<TestBackend>,
    is_active: bool,
}

impl Harness {
    fn new(
        protocol: ProtocolType,
        (width, height): (u16, u16),
        n_playlists: usize,
        loaded: fn(usize) -> bool,
    ) -> Self {
        init_config();
        let (tx, client_rx) = flume::unbounded();
        let state = Arc::new(State::new(tx, false, Arc::new(Mutex::new(VecDeque::new()))));
        {
            let mut data = state.data.write();
            data.user_data.playlists = (0..n_playlists)
                .map(|i| PlaylistFolderItem::Playlist(playlist(i)))
                .collect();
            for i in (0..n_playlists).filter(|i| loaded(*i)) {
                data.caches
                    .images
                    .insert(cover_url(i), cover_image(i), *TTL_CACHE_DURATION);
            }
        }
        {
            let mut ui = state.ui.lock();
            // the only constructor with a fixed font size, which keeps pixel sizes deterministic
            #[allow(deprecated)]
            let mut picker = Picker::from_fontsize((8, 16).into());
            picker.set_protocol_type(protocol);
            ui.picker = picker;
            ui.last_playlists_page_render_info.font_ratio = Some(0.5);
            ui.history = vec![PageState::Playlists {
                state: PlaylistsPageUIState::new(),
            }];
        }
        Self {
            state,
            client_rx,
            terminal: Terminal::new(TestBackend::new(width, height)).unwrap(),
            is_active: true,
        }
    }

    fn page_state<T>(&self, f: impl FnOnce(&mut PlaylistsPageUIState) -> T) -> T {
        let mut ui = self.state.ui.lock();
        match ui.current_page_mut() {
            PageState::Playlists { state } => f(state),
            _ => unreachable!("harness always shows the Playlists page"),
        }
    }

    /// Render one frame. The `ui` lock is released afterwards, since background encoders need it.
    fn draw(&mut self) -> (Buffer, Duration) {
        let (state, terminal, is_active) = (&self.state, &mut self.terminal, self.is_active);
        let mut ui = state.ui.lock();
        let start = Instant::now();
        let frame = terminal
            .draw(|f| {
                let area = f.area();
                super::page::render_playlists_page(is_active, f, state, &mut ui, area);
            })
            .unwrap();
        let elapsed = start.elapsed();
        (frame.buffer.clone(), elapsed)
    }

    /// Draw until two consecutive frames are identical and no cover work is pending.
    fn settle(&mut self) -> Vec<Buffer> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut frames: Vec<Buffer> = Vec::new();
        loop {
            let (buf, _) = self.draw();
            let stable = frames.last().is_some_and(|prev| *prev == buf);
            frames.push(buf);
            if stable && !self.has_pending_work() {
                return frames;
            }
            assert!(Instant::now() < deadline, "playlists page did not settle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn has_pending_work(&self) -> bool {
        self.state
            .ui
            .lock()
            .last_playlists_page_render_info
            .covers
            .has_pending()
    }

    fn encoded_covers(&self) -> usize {
        self.state
            .ui
            .lock()
            .last_playlists_page_render_info
            .covers
            .encoded
    }

    /// Height of one grid row (cover + title + gap) in cells.
    fn item_height(&self) -> f64 {
        let ui = self.state.ui.lock();
        f64::from(
            ui.last_playlists_page_render_info
                .layout
                .unwrap()
                .item_height,
        )
    }

    fn load_image_requests(&self) -> usize {
        self.client_rx
            .drain()
            .filter(|r| matches!(r, ClientRequest::LoadImage(_)))
            .count()
    }
}

#[derive(Clone, Copy)]
enum Scroll {
    /// Scroll by this many cells.
    Cells(f64),
    /// Scroll by whole grid rows plus some cells.
    Rows(f64, f64),
    /// Leave scrolling to the page's own "keep selection in view" logic.
    FollowSelection,
}

struct Scenario {
    name: &'static str,
    size: (u16, u16),
    selected: usize,
    scroll: Scroll,
    active: bool,
    query: Option<&'static str>,
    loaded: fn(usize) -> bool,
}

impl Scenario {
    fn new(name: &'static str, size: (u16, u16)) -> Self {
        Self {
            name,
            size,
            selected: 0,
            scroll: Scroll::Cells(0.0),
            active: true,
            query: None,
            loaded: |_| true,
        }
    }
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario::new("top", WIDE),
        Scenario {
            selected: 14,
            ..Scenario::new("sel_middle", WIDE)
        },
        Scenario {
            selected: 12,
            scroll: Scroll::Cells(1.0),
            ..Scenario::new("scrolled_1", WIDE)
        },
        Scenario {
            selected: 12,
            scroll: Scroll::Cells(4.0),
            ..Scenario::new("scrolled_4", WIDE)
        },
        Scenario {
            selected: 21,
            scroll: Scroll::Rows(1.0, 3.0),
            ..Scenario::new("row_plus_3", WIDE)
        },
        Scenario {
            selected: N_PLAYLISTS - 1,
            scroll: Scroll::FollowSelection,
            ..Scenario::new("bottom", WIDE)
        },
        Scenario {
            query: Some("zz"),
            ..Scenario::new("search_filtered", WIDE)
        },
        Scenario {
            active: false,
            ..Scenario::new("inactive", WIDE)
        },
        Scenario {
            loaded: |i| i % 3 != 0,
            ..Scenario::new("missing_images", WIDE)
        },
        Scenario::new("narrow_top", NARROW),
        Scenario {
            selected: 5,
            scroll: Scroll::Cells(4.0),
            ..Scenario::new("narrow_scrolled_4", NARROW)
        },
    ]
}

fn render_scenario(protocol: ProtocolType, s: &Scenario) -> Buffer {
    let mut h = Harness::new(protocol, s.size, N_PLAYLISTS, s.loaded);
    h.is_active = s.active;
    if let Some(query) = s.query {
        h.state.ui.lock().popup = Some(PopupState::Search {
            query: query.to_string(),
            input_focused: false,
        });
    }
    // the first frame computes the layout that scroll offsets depend on
    h.draw();
    let offset = match s.scroll {
        Scroll::Cells(c) => Some(c),
        Scroll::Rows(r, c) => Some(r * h.item_height() + c),
        Scroll::FollowSelection => None,
    };
    h.page_state(|p| {
        p.selected_index = s.selected;
        if let Some(offset) = offset {
            p.scroll_offset = offset;
            p.target_scroll_offset = offset;
        }
    });
    h.settle().pop().unwrap()
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ui/visual_test/golden")
}

fn output_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target"),
        PathBuf::from,
    );
    target.join("visual")
}

fn update_goldens() -> bool {
    std::env::var_os("SPOTIFY_PLAYER_UPDATE_GOLDENS").is_some()
}

/// Fraction of pixels allowed to differ before a halfblocks scenario fails. Small enough that a
/// single missing title (~0.03%) is caught.
const MAX_DIFF_FRACTION: f64 = 0.0002;

#[test]
fn playlists_page_halfblocks() {
    let golden_dir = golden_dir().join("halfblocks");
    let out_dir = output_dir().join("halfblocks");
    std::fs::create_dir_all(&golden_dir).unwrap();
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut failures = Vec::new();
    for s in scenarios() {
        let actual = raster::rasterize(&render_scenario(ProtocolType::Halfblocks, &s));
        actual
            .save(out_dir.join(format!("{}.png", s.name)))
            .unwrap();
        let golden_path = golden_dir.join(format!("{}.png", s.name));
        if update_goldens() {
            actual.save(&golden_path).unwrap();
            continue;
        }
        let Ok(golden) = image::open(&golden_path).map(|i| i.to_rgba8()) else {
            failures.push(format!(
                "{}: missing golden {}",
                s.name,
                golden_path.display()
            ));
            continue;
        };
        let (fraction, diff) = raster::compare(&golden, &actual);
        raster::side_by_side(&[&golden, &actual, &diff])
            .save(out_dir.join(format!("{}.compare.png", s.name)))
            .unwrap();
        if fraction > MAX_DIFF_FRACTION {
            failures.push(format!(
                "{}: {:.2}% of pixels differ",
                s.name,
                fraction * 100.0
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "visual differences (see {}):\n{}",
        out_dir.display(),
        failures.join("\n")
    );
}

#[test]
fn playlists_page_kitty() {
    let golden_dir = golden_dir().join("kitty");
    let out_dir = output_dir().join("kitty");
    std::fs::create_dir_all(&golden_dir).unwrap();
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut failures = Vec::new();
    for s in scenarios() {
        let actual = raster::kitty_snapshot(&render_scenario(ProtocolType::Kitty, &s));
        std::fs::write(out_dir.join(format!("{}.txt", s.name)), &actual).unwrap();
        let golden_path = golden_dir.join(format!("{}.txt", s.name));
        if update_goldens() {
            std::fs::write(&golden_path, &actual).unwrap();
            continue;
        }
        match std::fs::read_to_string(&golden_path) {
            Ok(golden) if golden == actual => {}
            Ok(golden) => {
                let first = golden
                    .lines()
                    .zip(actual.lines())
                    .position(|(g, a)| g != a)
                    .unwrap_or(0);
                failures.push(format!(
                    "{}: first difference on line {}",
                    s.name,
                    first + 1
                ));
            }
            Err(_) => failures.push(format!(
                "{}: missing golden {}",
                s.name,
                golden_path.display()
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "kitty snapshot differences (see {}):\n{}",
        out_dir.display(),
        failures.join("\n")
    );
}

#[derive(Default, Debug)]
struct PassStats {
    frames: usize,
    transmits: usize,
    graphics_bytes: usize,
    written_bytes: usize,
    max_written_bytes: usize,
    render_time: Duration,
    max_render_time: Duration,
}

/// Animate a scroll to `target` the way the UI thread does (a frame every 32ms) and measure what
/// would be written to the terminal.
fn animate_to(h: &mut Harness, target: f64, prev: &mut Buffer) -> PassStats {
    h.page_state(|p| p.target_scroll_offset = target);
    let mut stats = PassStats::default();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (buf, elapsed) = h.draw();
        let written: usize = prev
            .diff(&buf)
            .iter()
            .map(|(_, _, c)| c.symbol().len())
            .sum();
        let (transmits, graphics_bytes) = raster::kitty_transmits(&buf);
        stats.frames += 1;
        stats.transmits += transmits;
        stats.graphics_bytes += graphics_bytes;
        stats.written_bytes += written;
        stats.max_written_bytes = stats.max_written_bytes.max(written);
        stats.render_time += elapsed;
        stats.max_render_time = stats.max_render_time.max(elapsed);
        let done = *prev == buf
            && h.page_state(|p| (p.scroll_offset - p.target_scroll_offset).abs() < f64::EPSILON)
            && !h.has_pending_work();
        *prev = buf;
        if done {
            return stats;
        }
        assert!(Instant::now() < deadline, "scroll animation did not finish");
        std::thread::sleep(Duration::from_millis(32));
    }
}

fn print_stats(label: &str, s: &PassStats) {
    let frames = s.frames.max(1);
    println!(
        "{label}: {} frames, {} kitty transmits ({} KiB graphics), written {} KiB total / {} KiB max per frame, render {:.2?} avg / {:.2?} max",
        s.frames,
        s.transmits,
        s.graphics_bytes / 1024,
        s.written_bytes / 1024,
        s.max_written_bytes / 1024,
        s.render_time / frames as u32,
        s.max_render_time,
    );
}

/// Measures scrolling cost with the Kitty protocol: six rows down into unseen covers, then back up.
#[test]
fn playlists_page_scroll_perf() {
    const N: usize = 120;
    let mut h = Harness::new(ProtocolType::Kitty, WIDE, N, |_| true);
    let frames = h.settle();
    let initial_transmits: usize = frames.iter().map(|b| raster::kitty_transmits(b).0).sum();
    let mut prev = frames.last().unwrap().clone();
    let rows = 6.0 * h.item_height();
    h.load_image_requests();

    let down = animate_to(&mut h, rows, &mut prev);
    let up = animate_to(&mut h, 0.0, &mut prev);
    let load_requests = h.load_image_requests();

    print_stats("scroll down 6 rows", &down);
    print_stats("scroll back up", &up);
    println!(
        "LoadImage requests while scrolling: {load_requests}, covers encoded: {}",
        h.encoded_covers()
    );

    assert!(
        h.encoded_covers() <= N,
        "covers were encoded more than once: {} encodes for {N} covers",
        h.encoded_covers()
    );
    let transmits = initial_transmits + down.transmits + up.transmits;
    assert!(
        transmits <= N,
        "covers were sent to the terminal more than once ({transmits} transmits for {N} covers)"
    );
    assert_eq!(
        up.transmits, 0,
        "scrolling back over seen rows re-sent covers"
    );
    assert_eq!(load_requests, 0, "all images were already loaded");
}

/// Each missing cover image is requested from the client once, not on every frame.
#[test]
fn playlists_page_requests_missing_images_once() {
    let mut h = Harness::new(ProtocolType::Kitty, WIDE, N_PLAYLISTS, |i| i % 3 != 0);
    for _ in 0..30 {
        h.draw();
    }
    let mut urls: Vec<String> = h
        .client_rx
        .drain()
        .filter_map(|r| match r {
            ClientRequest::LoadImage(url) => Some(url),
            _ => None,
        })
        .collect();
    let requests = urls.len();
    urls.sort();
    urls.dedup();
    assert!(requests > 0, "missing images were never requested");
    assert_eq!(
        requests,
        urls.len(),
        "some images were requested repeatedly"
    );
}

/// Renders the real library from the local spotify_player cache, for eyeballing only.
#[test]
#[ignore = "needs a local spotify_player cache"]
fn playlists_page_real_library() {
    let cache = dirs_next::home_dir().unwrap().join(".cache/spotify-player");
    let playlists: Vec<PlaylistFolderItem> = serde_json::from_reader(std::io::BufReader::new(
        std::fs::File::open(cache.join("Playlists_cache.json")).unwrap(),
    ))
    .unwrap();

    let mut h = Harness::new(ProtocolType::Halfblocks, WIDE, 0, |_| false);
    {
        let mut data = h.state.data.write();
        for item in playlists.iter().take(60) {
            if let PlaylistFolderItem::Playlist(Playlist {
                cover_url: Some(url),
                ..
            }) = item
            {
                let path = cache.join("image").join(url.replace('/', ""));
                if let Ok(img) = std::fs::read(path)
                    .map_err(anyhow::Error::from)
                    .and_then(|b| image::load_from_memory(&b).map_err(anyhow::Error::from))
                {
                    data.caches
                        .images
                        .insert(url.clone(), img, *TTL_CACHE_DURATION);
                }
            }
        }
        data.user_data.playlists = playlists;
    }
    let out_dir = output_dir();
    std::fs::create_dir_all(&out_dir).unwrap();
    for (name, rows) in [("real_library_top", 0.0), ("real_library_scrolled", 1.3)] {
        h.draw();
        let offset = (rows * h.item_height()).round();
        h.page_state(|p| {
            p.scroll_offset = offset;
            p.target_scroll_offset = offset;
            p.selected_index = 12;
        });
        let buf = h.settle().pop().unwrap();
        raster::rasterize(&buf)
            .save(out_dir.join(format!("{name}.png")))
            .unwrap();
    }
}
