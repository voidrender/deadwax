//! Album art: picking an image protocol and drawing covers.
//!
//! One decoded cover is kept for the current track, and one for the info
//! popup. The strip and Now Playing each get
//! their own `StatefulProtocol`, because ratatui-image encodes a protocol for
//! one area size at a time. Resizing and encoding run on a blocking thread
//! when a resizer is attached (the runtime attaches one). Without it, as in
//! tests, they run inline.

use super::theme;
use super::widgets::centered;
use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{FilterType, FontSize, Resize, ResizeEncodeRender};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

/// Overrides `image_protocol` from the config file.
pub const PROTOCOL_ENV: &str = "DEADWAX_IMAGE_PROTOCOL";

/// How long the terminal gets to answer each read of the protocol query.
const QUERY_TIMEOUT: Duration = Duration::from_millis(1000);
/// Hard cap on the whole query, so startup never hangs on a silent terminal.
const QUERY_DEADLINE: Duration = Duration::from_secs(3);

/// Covers are scaled to fill their area, smoothing as they shrink.
const RESIZE: Resize = Resize::Scale(Some(FilterType::Triangle));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolChoice {
    Auto,
    Kitty,
    Iterm2,
    Sixel,
    Halfblocks,
}

/// Parses a protocol setting, case-insensitively. Missing means `Auto`;
/// anything unknown logs a warning and falls back to `Auto`.
pub fn parse_protocol(s: Option<&str>) -> ProtocolChoice {
    let Some(raw) = s.map(str::trim) else {
        return ProtocolChoice::Auto;
    };
    match raw.to_ascii_lowercase().as_str() {
        "" | "auto" => ProtocolChoice::Auto,
        "kitty" => ProtocolChoice::Kitty,
        "iterm2" => ProtocolChoice::Iterm2,
        "sixel" => ProtocolChoice::Sixel,
        "halfblocks" => ProtocolChoice::Halfblocks,
        other => {
            tracing::warn!("unknown image protocol {other:?}, using auto");
            ProtocolChoice::Auto
        }
    }
}

/// The raw protocol setting: `DEADWAX_IMAGE_PROTOCOL` wins over the config.
pub fn protocol_setting(env: Option<String>, config: Option<String>) -> Option<String> {
    env.filter(|s| !s.trim().is_empty()).or(config)
}

/// Builds the picker for `choice`. Every choice except `Halfblocks` queries
/// the terminal for its font size (and, for `Auto`, its protocol), so call it
/// after raw mode is on and before anything reads terminal events. Any error
/// or timeout falls back to half-blocks.
pub fn make_picker(choice: ProtocolChoice) -> Picker {
    let picker = match choice {
        ProtocolChoice::Halfblocks => halfblocks(),
        ProtocolChoice::Auto => query_picker().unwrap_or_else(halfblocks),
        forced => {
            let mut picker = query_picker().unwrap_or_else(guessed_font_picker);
            picker.set_protocol_type(match forced {
                ProtocolChoice::Kitty => ProtocolType::Kitty,
                ProtocolChoice::Iterm2 => ProtocolType::Iterm2,
                _ => ProtocolType::Sixel,
            });
            picker
        }
    };
    let FontSize { width, height } = picker.font_size();
    tracing::info!(
        "image protocol: {} (cell {width}x{height} px)",
        protocol_name(picker.protocol_type())
    );
    picker
}

pub fn protocol_name(p: ProtocolType) -> &'static str {
    match p {
        ProtocolType::Halfblocks => "halfblocks",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Kitty => "kitty",
        ProtocolType::Iterm2 => "iterm2",
    }
}

/// A half-block picker. Built once because construction checks the
/// environment for tmux.
fn halfblocks() -> Picker {
    static PICKER: OnceLock<Picker> = OnceLock::new();
    PICKER.get_or_init(Picker::halfblocks).clone()
}

/// Runs ratatui-image's stdio query on its own thread with a hard deadline.
/// If the deadline passes, the query thread may stay blocked on stdin and
/// swallow one later keypress, which beats hanging startup.
fn query_picker() -> Option<Picker> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("image-query".into())
        .spawn(move || {
            let options = QueryStdioOptions {
                timeout: QUERY_TIMEOUT,
                ..QueryStdioOptions::default()
            };
            let _ = tx.send(Picker::from_query_stdio_with_options(options));
        });
    if let Err(e) = spawned {
        tracing::warn!("image protocol query not started: {e}");
        return None;
    }
    match rx.recv_timeout(QUERY_DEADLINE) {
        Ok(Ok(picker)) => Some(picker),
        Ok(Err(e)) => {
            tracing::warn!("image protocol query failed: {e}");
            None
        }
        Err(_) => {
            tracing::warn!("image protocol query timed out");
            None
        }
    }
}

/// A picker for a forced protocol when the query failed: the cell size
/// comes from the window size in pixels, if the terminal reports it.
fn guessed_font_picker() -> Picker {
    let font = crossterm::terminal::window_size().ok().and_then(|w| {
        (w.width > 0 && w.height > 0 && w.columns > 0 && w.rows > 0)
            .then(|| FontSize::new(w.width / w.columns, w.height / w.rows))
    });
    match font {
        // The only constructor that takes a font size.
        #[allow(deprecated)]
        Some(font) => Picker::from_fontsize(font),
        None => halfblocks(),
    }
}

/// Where an image is drawn. Each slot keeps its own encoded protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Strip,
    Large,
    /// The info popup's cover, which may belong to any song or album.
    Info,
}

/// A resize and encode to run off the UI thread.
pub struct ResizeJob {
    slot: Slot,
    generation: u64,
    size: Size,
    protocol: StatefulProtocol,
}

impl ResizeJob {
    /// Does the blocking work. Call it from `spawn_blocking`.
    pub fn run(mut self) -> ResizeDone {
        self.protocol.resize_encode(&RESIZE, self.size);
        let ok = match self.protocol.last_encoding_result() {
            Some(Err(e)) => {
                tracing::warn!("encoding cover art failed: {e}");
                false
            }
            _ => true,
        };
        ResizeDone {
            slot: self.slot,
            generation: self.generation,
            size: self.size,
            protocol: self.protocol,
            ok,
        }
    }
}

/// A finished `ResizeJob`, handed back to `ArtState::finish_resize`.
pub struct ResizeDone {
    slot: Slot,
    generation: u64,
    size: Size,
    protocol: StatefulProtocol,
    ok: bool,
}

impl std::fmt::Debug for ResizeDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResizeDone")
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .field("size", &self.size)
            .field("ok", &self.ok)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct SlotState {
    protocol: Option<StatefulProtocol>,
    /// A resize is out on the blocking pool; the protocol comes back with it.
    pending: bool,
    /// The size whose encode failed, so it is not retried every frame.
    failed: Option<Size>,
}

impl SlotState {
    fn has_image(&self) -> bool {
        self.protocol.is_some() || self.pending
    }
}

/// The covers for the current track and the info popup, and their
/// per-area protocols.
pub struct ArtState {
    pub picker: Picker,
    /// The cover the current track wants, whether or not it has arrived.
    pub cover_id: Option<String>,
    /// The current track's decoded cover, so the info popup can reuse it.
    image: Option<DynamicImage>,
    strip: SlotState,
    large: SlotState,
    /// Bumped whenever the image changes, so stale resizes are dropped.
    generation: u64,
    /// The cover the info popup wants.
    pub info_cover_id: Option<String>,
    info: SlotState,
    info_generation: u64,
    resizer: Option<UnboundedSender<ResizeJob>>,
    /// An overlay is open: draw placeholders instead of images, so no image
    /// escape (such as kitty's one-time upload) lands in a covered cell.
    suppressed: bool,
    drawn: Vec<(Slot, Rect, u64)>,
    last_drawn: Vec<(Slot, Rect, u64)>,
    needs_clear: bool,
}

impl Default for ArtState {
    fn default() -> Self {
        Self::new(halfblocks())
    }
}

impl std::fmt::Debug for ArtState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtState")
            .field("protocol", &protocol_name(self.picker.protocol_type()))
            .field("cover_id", &self.cover_id)
            .field("has_image", &self.has_image())
            .field("info_cover_id", &self.info_cover_id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl ArtState {
    pub fn new(picker: Picker) -> Self {
        Self {
            picker,
            cover_id: None,
            image: None,
            strip: SlotState::default(),
            large: SlotState::default(),
            generation: 0,
            info_cover_id: None,
            info: SlotState::default(),
            info_generation: 0,
            resizer: None,
            suppressed: false,
            drawn: Vec::new(),
            last_drawn: Vec::new(),
            needs_clear: false,
        }
    }

    /// Swaps the picker. Any image is dropped, since it was encoded for the
    /// old one.
    pub fn set_picker(&mut self, picker: Picker) {
        self.picker = picker;
        self.drop_image();
        self.drop_info();
    }

    /// Sends resizes to a worker instead of running them on the UI thread.
    pub fn set_resizer(&mut self, tx: UnboundedSender<ResizeJob>) {
        self.resizer = Some(tx);
    }

    /// Records the cover the current track wants. Returns true when it
    /// differs from the last one, and drops the old image if so.
    pub fn expect(&mut self, cover_id: String) -> bool {
        if self.cover_id.as_deref() == Some(cover_id.as_str()) {
            return false;
        }
        self.cover_id = Some(cover_id);
        self.drop_image();
        true
    }

    /// Records the cover the info popup wants. Returns true when it has to
    /// be loaded. The current track's cover is reused when it is the same.
    pub fn expect_info(&mut self, cover_id: String) -> bool {
        if self.info_cover_id.as_deref() == Some(cover_id.as_str()) {
            return false;
        }
        self.drop_info();
        let reuse = (self.cover_id.as_deref() == Some(cover_id.as_str()))
            .then(|| self.image.clone())
            .flatten();
        self.info_cover_id = Some(cover_id);
        match reuse {
            Some(img) => {
                self.info.protocol = Some(self.picker.new_resize_protocol(img));
                false
            }
            None => true,
        }
    }

    /// The info popup closed, or shows something without a cover.
    pub fn clear_info(&mut self) {
        if self.info_cover_id.is_some() || self.info.protocol.is_some() {
            self.info_cover_id = None;
            self.drop_info();
        }
    }

    /// A load of `cover_id` failed. When it is still the wanted cover, forget
    /// it so a later `expect` asks for it again. Returns whether the current
    /// track wanted it.
    pub fn forget(&mut self, cover_id: &str) -> bool {
        if self.info_cover_id.as_deref() == Some(cover_id) {
            self.info_cover_id = None;
        }
        if self.cover_id.as_deref() != Some(cover_id) {
            return false;
        }
        self.cover_id = None;
        true
    }

    /// The current track has no cover: back to the placeholder.
    pub fn clear(&mut self) {
        if self.cover_id.is_some() || self.has_image() {
            self.cover_id = None;
            self.drop_image();
        }
    }

    /// Installs a decoded cover wherever it is wanted: the current track,
    /// the info popup, or both. It is ignored elsewhere, so a late load
    /// never shows the wrong art, and a second load of a cover that is
    /// already shown changes nothing.
    pub fn set_image(&mut self, cover_id: &str, img: DynamicImage) {
        if self.info_cover_id.as_deref() == Some(cover_id) && !self.info.has_image() {
            self.info.protocol = Some(self.picker.new_resize_protocol(img.clone()));
        }
        if self.cover_id.as_deref() != Some(cover_id) || self.has_image() {
            return;
        }
        self.drop_image();
        self.strip.protocol = Some(self.picker.new_resize_protocol(img.clone()));
        self.large.protocol = Some(self.picker.new_resize_protocol(img.clone()));
        self.image = Some(img);
    }

    /// Whether the current track's cover has arrived.
    pub fn has_image(&self) -> bool {
        self.strip.has_image() || self.large.has_image()
    }

    /// Whether the info popup's cover has arrived.
    pub fn has_info_image(&self) -> bool {
        self.info.has_image()
    }

    fn drop_image(&mut self) {
        self.strip = SlotState::default();
        self.large = SlotState::default();
        self.image = None;
        self.generation = self.generation.wrapping_add(1);
    }

    fn drop_info(&mut self) {
        self.info = SlotState::default();
        self.info_generation = self.info_generation.wrapping_add(1);
    }

    fn slot_mut(&mut self, slot: Slot) -> &mut SlotState {
        match slot {
            Slot::Strip => &mut self.strip,
            Slot::Large => &mut self.large,
            Slot::Info => &mut self.info,
        }
    }

    fn generation(&self, slot: Slot) -> u64 {
        match slot {
            Slot::Strip | Slot::Large => self.generation,
            Slot::Info => self.info_generation,
        }
    }

    /// Takes back a protocol from the resize worker. Results for an image
    /// that has since been replaced are dropped.
    pub fn finish_resize(&mut self, done: ResizeDone) {
        if done.generation != self.generation(done.slot) {
            return;
        }
        let state = self.slot_mut(done.slot);
        state.pending = false;
        state.failed = (!done.ok).then_some(done.size);
        state.protocol = Some(done.protocol);
    }

    /// Call before drawing a frame. `overlay` is true when a popup or the
    /// help screen is open. The info popup's own cover still draws.
    pub fn begin_frame(&mut self, overlay: bool) {
        self.suppressed = overlay;
        self.drawn.clear();
    }

    /// Call after drawing a frame. Sixel and iTerm2 images are pixels in the
    /// terminal, outside ratatui's buffer, so when one is moved or removed
    /// the screen must be cleared and redrawn. Kitty placements vanish with
    /// their placeholder cells, and half-blocks are plain cells.
    pub fn end_frame(&mut self) {
        let gone = self.last_drawn.iter().any(|d| !self.drawn.contains(d));
        let pixels = matches!(
            self.picker.protocol_type(),
            ProtocolType::Sixel | ProtocolType::Iterm2
        );
        if gone && pixels {
            self.needs_clear = true;
        }
        self.last_drawn = std::mem::take(&mut self.drawn);
    }

    /// Whether the runtime should clear the terminal and draw again.
    pub fn take_needs_clear(&mut self) -> bool {
        std::mem::take(&mut self.needs_clear)
    }

    /// Draws the image for `slot` centered in `area`, or the placeholder
    /// while there is none (no cover, still loading, or being resized).
    pub fn render(&mut self, frame: &mut Frame, area: Rect, slot: Slot) {
        if area.is_empty() {
            return;
        }
        if self.suppressed && slot != Slot::Info {
            draw_placeholder(frame, area);
            return;
        }
        let generation = self.generation(slot);
        let resizer = self.resizer.clone();
        let state = self.slot_mut(slot);
        let Some(mut protocol) = state.protocol.take() else {
            draw_placeholder(frame, area);
            return;
        };
        if let Some(size) = protocol.needs_resize(&RESIZE, area.as_size()) {
            if state.failed == Some(size) {
                state.protocol = Some(protocol);
                draw_placeholder(frame, area);
                return;
            }
            let job = ResizeJob {
                slot,
                generation,
                size,
                protocol,
            };
            let job = match resizer {
                Some(tx) => match tx.send(job) {
                    Ok(()) => {
                        state.pending = true;
                        draw_placeholder(frame, area);
                        return;
                    }
                    // The worker is gone: do the work here instead.
                    Err(e) => e.0,
                },
                None => job,
            };
            let done = job.run();
            state.failed = (!done.ok).then_some(done.size);
            protocol = done.protocol;
            if !done.ok {
                state.protocol = Some(protocol);
                draw_placeholder(frame, area);
                return;
            }
        }
        let size = protocol.size_for(RESIZE, area.as_size());
        // The strip's cover sits flush left, next to the track text.
        let rect = match slot {
            Slot::Strip => {
                let row = centered(area, area.width, size.height);
                Rect {
                    width: size.width.min(area.width),
                    ..row
                }
            }
            Slot::Large | Slot::Info => centered(area, size.width, size.height),
        };
        protocol.render(rect, frame.buffer_mut());
        state.protocol = Some(protocol);
        self.drawn.push((slot, rect, generation));
    }
}

/// A rounded box with a centered note, shown wherever art is missing.
pub fn draw_placeholder(frame: &mut Frame, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new("♪").style(theme::dim()).centered(),
        centered(inner, inner.width, 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testing::buffer_text;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn protocol_env_overrides_config() {
        assert_eq!(
            parse_protocol(Some("halfblocks")),
            ProtocolChoice::Halfblocks
        );
        assert_eq!(parse_protocol(None), ProtocolChoice::Auto);
        assert_eq!(parse_protocol(Some("KiTTy")), ProtocolChoice::Kitty);
        assert_eq!(parse_protocol(Some("bogus")), ProtocolChoice::Auto);
        assert_eq!(
            protocol_setting(Some("sixel".into()), Some("kitty".into())).as_deref(),
            Some("sixel")
        );
        assert_eq!(
            protocol_setting(None, Some("kitty".into())).as_deref(),
            Some("kitty")
        );
    }

    #[test]
    fn halfblocks_choice_never_queries_the_terminal() {
        let picker = make_picker(ProtocolChoice::Halfblocks);
        assert_eq!(picker.protocol_type(), ProtocolType::Halfblocks);
    }

    fn image() -> DynamicImage {
        // A gradient, because a flat color renders as plain spaces.
        image::RgbImage::from_fn(40, 40, |x, y| {
            image::Rgb([
                u8::try_from(x * 6).unwrap(),
                u8::try_from(y * 6).unwrap(),
                90,
            ])
        })
        .into()
    }

    fn draw(art: &mut ArtState, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            art.begin_frame(false);
            art.render(f, f.area(), Slot::Large);
            art.end_frame();
        })
        .unwrap();
        buffer_text(term.backend().buffer())
    }

    #[test]
    fn resizes_go_to_the_worker_and_stale_results_are_dropped() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut art = ArtState::default();
        art.set_resizer(tx);
        art.expect("a".into());
        art.set_image("a", image());

        // The first frame hands the work off and shows the placeholder.
        assert!(draw(&mut art, 12, 6).contains('♪'));
        let done = rx.try_recv().unwrap().run();
        art.finish_resize(done);
        let text = draw(&mut art, 12, 6);
        assert!(text.contains(['▀', '▄']), "{text}");

        // A new size goes out again; a cover change makes its result stale.
        draw(&mut art, 20, 10);
        let done = rx.try_recv().unwrap().run();
        art.expect("b".into());
        art.finish_resize(done);
        assert!(!art.has_image());
    }

    #[test]
    fn strip_art_is_flush_left_and_large_art_is_centered() {
        for (slot, flush) in [(Slot::Strip, true), (Slot::Large, false)] {
            let mut art = ArtState::new(halfblocks());
            art.expect("a".into());
            art.set_image("a", image());
            let mut term = Terminal::new(TestBackend::new(40, 6)).unwrap();
            term.draw(|f| {
                art.begin_frame(false);
                art.render(f, f.area(), slot);
                art.end_frame();
            })
            .unwrap();
            let rect = art.last_drawn[0].1;
            assert!(rect.width < 40, "{rect:?}");
            assert_eq!(rect.x == 0, flush, "{slot:?} {rect:?}");
        }
    }

    #[test]
    fn overlays_suppress_the_image() {
        let mut art = ArtState::default();
        art.expect("a".into());
        art.set_image("a", image());
        let mut term = Terminal::new(TestBackend::new(12, 6)).unwrap();
        term.draw(|f| {
            art.begin_frame(true);
            art.render(f, f.area(), Slot::Strip);
            art.end_frame();
        })
        .unwrap();
        assert!(buffer_text(term.backend().buffer()).contains('♪'));
    }

    #[test]
    fn moving_a_sixel_image_asks_for_a_clear() {
        let mut picker = halfblocks();
        picker.set_protocol_type(ProtocolType::Sixel);
        let mut art = ArtState::new(picker);
        art.expect("a".into());
        art.set_image("a", image());
        draw(&mut art, 12, 6);
        assert!(!art.take_needs_clear());
        draw(&mut art, 12, 6);
        assert!(!art.take_needs_clear());
        art.clear();
        draw(&mut art, 12, 6);
        assert!(art.take_needs_clear());
    }
}
