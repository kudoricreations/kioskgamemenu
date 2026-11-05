//! The SDL session: display, input loop and drawing.
//!
//! [`run_session`] owns every SDL object as a local variable. Returning from
//! it therefore drops them all and ends in `SDL_Quit`, which closes the DRM
//! file descriptor and so releases DRM master. Hiding or minimising the window
//! would not do that. The launcher relies on this: by the time `run_session`
//! returns, the display is free for the emulator.
//!
//! Local variables drop in reverse declaration order, so the declaration order
//! in `run_session` also guarantees textures die before their renderer and the
//! renderer before the video subsystem.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sdl2::VideoSubsystem;
use sdl2::event::{Event, WindowEvent};
use sdl2::hint;
use sdl2::image::{InitFlag, LoadSurface};
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::{BlendMode, Texture, TextureCreator, WindowCanvas};
use sdl2::surface::Surface;
use sdl2::ttf::{Font, Sdl2TtfContext};
use sdl2::video::WindowContext;

use crate::audio::Audio;
use crate::config::{AppearanceConfig, Config};
use crate::gamepad::{Action, Input};
use crate::launcher::LaunchPlan;
use crate::menu::{Area, Layout, MenuState, cover_crop, fit_inside};

const WINDOWED_SIZE: (u32, u32) = (1280, 720);
/// Longest side of a cover texture, keeping huge scans from filling VRAM.
const MAX_COVER_SIDE: u32 = 1024;
const MAX_ICON_SIDE: u32 = 128;
const MAX_BACKGROUND_SIDE: u32 = 3840;
const MAX_BANNER_SIDE: u32 = 1920;
/// Darkness of the strips behind text when a wallpaper is showing.
const TEXT_BACKDROP_ALPHA: u8 = 150;
/// Decoding is synchronous, so a budget keeps scrolling responsive: covers
/// appear over a few frames instead of stalling input.
const COVER_LOADS_PER_FRAME: usize = 2;
const MIN_CACHED_COVERS: usize = 48;
const MAX_CACHED_TEXT: usize = 256;
/// Ignore launches right after the menu appears, so that buttons mashed while
/// quitting an emulator cannot immediately start the game again.
const LAUNCH_GUARD: Duration = Duration::from_millis(750);
const NOTICE_DURATION: Duration = Duration::from_secs(8);

const FALLBACK_FONTS: &[&str] = &[
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

const BACKGROUND: Color = Color::RGB(14, 16, 22);
const PANEL: Color = Color::RGB(30, 34, 44);
const ACCENT: Color = Color::RGB(255, 196, 64);
const TEXT: Color = Color::RGB(236, 238, 242);
const MUTED: Color = Color::RGB(150, 156, 168);
const ERROR: Color = Color::RGB(168, 40, 48);
const INFO: Color = Color::RGB(52, 76, 120);
/// Placeholder cover colours, picked by hashing the game ID.
const PLACEHOLDERS: [Color; 6] = [
    Color::RGB(58, 74, 120),
    Color::RGB(96, 56, 112),
    Color::RGB(40, 98, 96),
    Color::RGB(120, 66, 52),
    Color::RGB(62, 96, 58),
    Color::RGB(112, 58, 76),
];

pub enum SessionEnd {
    Quit,
    Escape,
    Launch(LaunchPlan),
}

/// Runs the menu until the player launches a game or the process is asked to
/// quit. `on_select` validates the selected game and records its statistics
/// while the menu is still up, so that a game which cannot start only
/// produces a message.
///
/// An `Err` means SDL or the display could not be initialised. All SDL
/// resources have been released whichever way this returns.
pub fn run_session(
    config: &Config,
    menu: &mut MenuState,
    on_select: &mut dyn FnMut(&mut MenuState) -> Option<LaunchPlan>,
) -> Result<SessionEnd, String> {
    // Hints do not survive SDL_Quit, so these are set for every session. Going
    // through the hint rather than the environment keeps the setting out of
    // the game's environment, and sdl2-compat forwards it to SDL3. A
    // SDL_VIDEODRIVER environment variable takes priority over this hint.
    if config.display.backend != "auto" {
        hint::set("SDL_VIDEODRIVER", &config.display.backend);
    }
    hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
    hint::set("SDL_RENDER_SCALE_QUALITY", "linear");

    let sdl = sdl2::init().map_err(|e| format!("SDL initialization failed: {e}"))?;
    let video = sdl
        .video()
        .map_err(|e| format!("video initialization failed: {e}"))?;
    let controllers = sdl
        .game_controller()
        .map_err(|e| format!("controller initialization failed: {e}"))?;
    let _image = sdl2::image::init(InitFlag::PNG | InitFlag::JPG)
        .inspect_err(|e| log_warn!("some image formats are unavailable: {e}"))
        .ok();
    let ttf = sdl2::ttf::init()
        .inspect_err(|e| log_warn!("font support unavailable, text disabled: {e}"))
        .ok();

    let canvas = create_canvas(&video, config.display.fullscreen)?;
    log_info!(
        "display ready: driver {}, renderer {}, output {:?}",
        video.current_video_driver(),
        canvas.info().name,
        canvas.output_size().unwrap_or_default(),
    );
    sdl.mouse().show_cursor(false);

    let creator = canvas.texture_creator();
    let mut event_pump = sdl
        .event_pump()
        .map_err(|e| format!("event initialization failed: {e}"))?;
    let mut input = Input::new(controllers);
    let mut renderer = Renderer::new(canvas, &creator, ttf.as_ref(), config);
    let audio = Audio::open(&sdl, &config.audio);
    if let Some(audio) = &audio {
        audio.start_music();
    }

    let started = Instant::now();
    let mut first_frame = true;
    let mut notice_deadline = None;
    let mut dirty = true;
    let mut events = Vec::new();
    let mut actions = Vec::new();

    let end = 'session: loop {
        let timeout = if dirty {
            0
        } else if input.is_repeating() {
            10
        } else {
            100
        };
        events.extend(event_pump.wait_event_timeout(timeout));
        events.extend(event_pump.poll_iter());

        let now = started.elapsed().as_millis() as u64;
        let connected = input.connected();
        for event in events.drain(..) {
            match event {
                Event::Quit { .. } => break 'session SessionEnd::Quit,
                Event::Window { win_event, .. } => {
                    dirty |= matches!(
                        win_event,
                        WindowEvent::Exposed
                            | WindowEvent::Resized(..)
                            | WindowEvent::SizeChanged(..)
                    );
                }
                Event::RenderDeviceReset { .. } | Event::RenderTargetsReset { .. } => {
                    renderer.forget_textures();
                    dirty = true;
                }
                other => input.handle_event(&other, now, &mut actions),
            }
        }
        input.tick(now, &mut actions);
        dirty |= input.connected() != connected;

        let mut selection_moved = false;
        for action in actions.drain(..) {
            dirty = true;
            match action {
                Action::Navigate(nav) => selection_moved |= menu.navigate(nav),
                Action::CycleSort => menu.cycle_sort(),
                Action::Launch if started.elapsed() >= LAUNCH_GUARD => {
                    menu.clear_notice();
                    notice_deadline = None;
                    if let Some(plan) = on_select(menu) {
                        if let Some(audio) = &audio {
                            audio.finish_for_launch();
                        }
                        break 'session SessionEnd::Launch(plan);
                    }
                }
                Action::Launch => {}
                Action::Escape => break 'session SessionEnd::Escape,
            }
        }
        if selection_moved {
            if let Some(audio) = &audio {
                audio.play_select();
            }
        }

        if menu.notice().is_some() {
            let deadline = *notice_deadline.get_or_insert_with(|| Instant::now() + NOTICE_DURATION);
            if Instant::now() >= deadline {
                menu.clear_notice();
                notice_deadline = None;
                dirty = true;
            }
        }

        if dirty {
            if first_frame {
                sdl2::clear_error();
            }
            dirty = renderer.render(menu, input.connected());
            if first_frame {
                first_frame = false;
                // Presenting cannot report failure, so a display that rejects
                // our framebuffer otherwise just leaves the console on screen.
                // SDL's error text also holds harmless messages, hence the match.
                let error = sdl2::get_error();
                if error.contains("videomode") {
                    log_warn!("the display rejected the first frame: {error}");
                }
            }
        }
    };

    Ok(end)
}

fn create_canvas(video: &VideoSubsystem, fullscreen: bool) -> Result<WindowCanvas, String> {
    let (width, height) = if fullscreen {
        video
            .desktop_display_mode(0)
            .map(|mode| (mode.w.max(1) as u32, mode.h.max(1) as u32))
            .unwrap_or(WINDOWED_SIZE)
    } else {
        WINDOWED_SIZE
    };
    let build_window = || {
        let mut builder = video.window("KioskGameMenu", width, height);
        builder.position_centered();
        if fullscreen {
            builder.fullscreen_desktop();
        } else {
            builder.resizable();
        }
        builder
            .build()
            .map_err(|e| format!("window creation failed: {e}"))
    };

    match build_window()?
        .into_canvas()
        .accelerated()
        .present_vsync()
        .build()
    {
        Ok(canvas) => Ok(canvas),
        Err(accelerated) => {
            log_warn!("accelerated renderer unavailable ({accelerated}), trying software");
            build_window()?
                .into_canvas()
                .software()
                .build()
                .map_err(|e| format!("renderer creation failed: {e}"))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum FontSize {
    Display,
    Heading,
    Body,
    Small,
}

struct Fonts<'t> {
    display: Font<'t, 'static>,
    heading: Font<'t, 'static>,
    body: Font<'t, 'static>,
    small: Font<'t, 'static>,
}

impl Fonts<'_> {
    fn get(&self, size: FontSize) -> &Font<'_, 'static> {
        match size {
            FontSize::Display => &self.display,
            FontSize::Heading => &self.heading,
            FontSize::Body => &self.body,
            FontSize::Small => &self.small,
        }
    }
}

fn load_fonts<'t>(ttf: &'t Sdl2TtfContext, configured: &Path, height: u32) -> Option<Fonts<'t>> {
    let fallbacks = FALLBACK_FONTS.iter().map(Path::new);
    let point = |divisor: u32| (height / divisor).max(9) as u16;
    for path in std::iter::once(configured).chain(fallbacks) {
        if !path.is_file() {
            continue;
        }
        let load = |size: u16| ttf.load_font(path, size);
        match (
            load(point(10)),
            load(point(24)),
            load(point(36)),
            load(point(46)),
        ) {
            (Ok(display), Ok(heading), Ok(body), Ok(small)) => {
                return Some(Fonts {
                    display,
                    heading,
                    body,
                    small,
                });
            }
            (Err(e), ..) => log_warn!("cannot load font {}: {e}", path.display()),
            _ => log_warn!("cannot load font {}", path.display()),
        }
    }
    log_warn!("no usable font found; set display.font in config.toml");
    None
}

struct CachedCover<'c> {
    /// `None` when the file is missing or unreadable, so it is not retried.
    texture: Option<Texture<'c>>,
    last_used: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TextKey {
    text: String,
    size: FontSize,
    color: (u8, u8, u8),
    max_width: u32,
}

struct Renderer<'c, 't> {
    canvas: WindowCanvas,
    creator: &'c TextureCreator<WindowContext>,
    ttf: Option<&'t Sdl2TtfContext>,
    font_path: PathBuf,
    fonts: Option<Fonts<'t>>,
    platforms_directory: PathBuf,
    appearance: &'t AppearanceConfig,
    layout: Option<Layout>,
    /// The outer `Option` means "not loaded yet", the inner one "no image".
    background: Option<Option<Texture<'c>>>,
    banner: Option<Option<Texture<'c>>>,
    covers: HashMap<String, CachedCover<'c>>,
    icons: HashMap<String, Option<Texture<'c>>>,
    text: HashMap<TextKey, Texture<'c>>,
    frame: u64,
}

impl<'c, 't> Renderer<'c, 't> {
    fn new(
        mut canvas: WindowCanvas,
        creator: &'c TextureCreator<WindowContext>,
        ttf: Option<&'t Sdl2TtfContext>,
        config: &'t Config,
    ) -> Self {
        canvas.set_blend_mode(BlendMode::Blend);
        Self {
            canvas,
            creator,
            ttf,
            font_path: config.display.font.clone(),
            fonts: None,
            platforms_directory: config.games.platforms_directory.clone(),
            appearance: &config.appearance,
            layout: None,
            background: None,
            banner: None,
            covers: HashMap::new(),
            icons: HashMap::new(),
            text: HashMap::new(),
            frame: 0,
        }
    }

    fn forget_textures(&mut self) {
        self.background = None;
        self.banner = None;
        self.covers.clear();
        self.icons.clear();
        self.text.clear();
    }

    /// Recomputes geometry when the output size changes.
    fn update_layout(&mut self, menu: &mut MenuState) -> Layout {
        let (width, height) = self.canvas.output_size().unwrap_or(WINDOWED_SIZE);
        if self.banner.is_none() {
            let path = &self.appearance.banner;
            self.banner = Some(self.load_image(path, MAX_BANNER_SIDE));
        }
        let has_banner = matches!(self.banner, Some(Some(_)));
        let current = Layout::compute(width, height, has_banner);
        if self.layout != Some(current) {
            self.layout = Some(current);
            menu.set_viewport(current.columns, current.rows);
            self.text.clear();
            self.fonts = self
                .ttf
                .and_then(|ttf| load_fonts(ttf, &self.font_path, current.height));
        }
        current
    }

    /// Draws one frame. Returns true if artwork is still loading, meaning
    /// another frame is needed.
    fn render(&mut self, menu: &mut MenuState, controllers: usize) -> bool {
        self.frame += 1;
        let layout = self.update_layout(menu);
        self.canvas.set_draw_color(BACKGROUND);
        self.canvas.clear();
        self.draw_background(layout);

        let loading = if menu.is_empty() {
            self.draw_empty_state(menu, layout);
            false
        } else {
            self.draw_grid(menu, layout)
        };
        self.draw_header(menu, layout);
        self.draw_footer(menu, layout, controllers);
        self.draw_notice(menu, layout);
        self.canvas.present();
        self.evict(menu);
        loading
    }

    fn has_wallpaper(&self) -> bool {
        matches!(self.background, Some(Some(_)))
    }

    /// Draws the wallpaper, cropped to cover the screen, and the shading that
    /// keeps the header and footer readable on top of it.
    fn draw_background(&mut self, layout: Layout) {
        if self.background.is_none() {
            let path = &self.appearance.background;
            self.background = Some(self.load_image(path, MAX_BACKGROUND_SIDE));
        }
        let Some(Some(texture)) = &self.background else {
            return;
        };
        let query = texture.query();
        let crop = cover_crop(query.width, query.height, layout.width, layout.height);
        let _ = self.canvas.copy(texture, rect(crop), None);

        let dim = self.appearance.background_dim * 255 / 100;
        self.canvas.set_draw_color(Color::RGBA(0, 0, 0, dim as u8));
        let _ = self.canvas.fill_rect(None);

        self.draw_text_backdrop(Area {
            x: 0,
            y: 0,
            width: layout.width,
            height: layout.header_height,
        });
        self.draw_text_backdrop(Area {
            x: 0,
            y: (layout.height - layout.footer_height) as i32,
            width: layout.width,
            height: layout.footer_height,
        });
    }

    fn draw_text_backdrop(&mut self, area: Area) {
        self.canvas
            .set_draw_color(Color::RGBA(0, 0, 0, TEXT_BACKDROP_ALPHA));
        let _ = self.canvas.fill_rect(rect(area));
    }

    fn draw_grid(&mut self, menu: &MenuState, layout: Layout) -> bool {
        let visible = menu.visible_positions();
        let mut budget = COVER_LOADS_PER_FRAME;
        let mut loading = false;
        for (slot, position) in visible.enumerate() {
            let Some(game) = menu.game_at(position) else {
                continue;
            };
            if !self.covers.contains_key(&game.id) {
                if budget == 0 {
                    loading = true;
                } else {
                    budget -= 1;
                    let texture = self.load_image(&game.cover, MAX_COVER_SIDE);
                    self.covers.insert(
                        game.id.clone(),
                        CachedCover {
                            texture,
                            last_used: 0,
                        },
                    );
                }
            }
            let area = layout.cover_area(slot);
            self.draw_cover(&game.id, &game.title, area);
            self.draw_platform_icon(&game.platform, area, layout);

            if self.has_wallpaper() {
                self.draw_text_backdrop(Area {
                    x: area.x,
                    y: area.y + area.height as i32,
                    width: area.width,
                    height: layout.title_height,
                });
            }
            let selected = position == menu.selected_position();
            let color = if selected { ACCENT } else { TEXT };
            let width = area.width.saturating_sub(4);
            self.draw_text(
                &game.title,
                FontSize::Body,
                color,
                area.x + 2,
                area.y + area.height as i32 + 6,
                width,
            );
            if selected {
                self.draw_selection(area, layout.title_height);
            }
        }
        self.draw_scrollbar(menu, layout);
        loading
    }

    fn draw_cover(&mut self, id: &str, title: &str, area: Area) {
        self.canvas.set_draw_color(PANEL);
        let _ = self.canvas.fill_rect(rect(area));
        let frame = self.frame;
        let mut drawn = false;
        if let Some(cached) = self.covers.get_mut(id) {
            cached.last_used = frame;
            if let Some(texture) = &cached.texture {
                let query = texture.query();
                let target = fit_inside(query.width, query.height, area);
                let _ = self.canvas.copy(texture, None, rect(target));
                drawn = true;
            }
        }
        if !drawn {
            self.draw_placeholder(id, title, area);
        }
    }

    /// Generated artwork for games without a usable cover image.
    fn draw_placeholder(&mut self, id: &str, title: &str, area: Area) {
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        let color = PLACEHOLDERS[hasher.finish() as usize % PLACEHOLDERS.len()];
        self.canvas.set_draw_color(color);
        let _ = self.canvas.fill_rect(rect(area));
        let initials: String = title
            .split_whitespace()
            .filter_map(|word| word.chars().next())
            .filter(|c| c.is_alphanumeric())
            .take(3)
            .collect::<String>()
            .to_uppercase();
        self.draw_text_centered(&initials, FontSize::Display, TEXT, area);
    }

    fn draw_platform_icon(&mut self, platform: &str, cover: Area, layout: Layout) {
        let side = (layout.cover_height / 7).max(16);
        let inset = 6;
        let badge = Area {
            x: cover.x + cover.width as i32 - side as i32 - inset,
            y: cover.y + cover.height as i32 - side as i32 - inset,
            width: side,
            height: side,
        };
        if !self.icons.contains_key(platform) {
            let path = self.platforms_directory.join(format!("{platform}.png"));
            let icon = self.load_image(&path, MAX_ICON_SIDE);
            self.icons.insert(platform.to_string(), icon);
        }
        self.canvas.set_draw_color(Color::RGBA(0, 0, 0, 170));
        let _ = self.canvas.fill_rect(rect(badge));
        if let Some(Some(texture)) = self.icons.get(platform) {
            let query = texture.query();
            let target = fit_inside(query.width, query.height, badge);
            let _ = self.canvas.copy(texture, None, rect(target));
        } else {
            let label: String = platform.chars().take(4).collect::<String>().to_uppercase();
            self.draw_text_centered(&label, FontSize::Small, TEXT, badge);
        }
    }

    fn draw_selection(&mut self, cover: Area, title_height: u32) {
        let thickness = 5;
        self.canvas.set_draw_color(ACCENT);
        let outer = Rect::new(
            cover.x - thickness,
            cover.y - thickness,
            cover.width + 2 * thickness as u32,
            cover.height + title_height + 2 * thickness as u32,
        );
        for inset in 0..thickness {
            let ring = Rect::new(
                outer.x() + inset,
                outer.y() + inset,
                outer.width() - 2 * inset as u32,
                outer.height() - 2 * inset as u32,
            );
            let _ = self.canvas.draw_rect(ring);
        }
    }

    fn draw_scrollbar(&mut self, menu: &MenuState, layout: Layout) {
        let total = menu.total_rows() as u32;
        if total <= layout.rows as u32 {
            return;
        }
        let track_height = layout.height - layout.header_height - layout.footer_height;
        let thumb = (track_height * layout.rows as u32 / total).max(12);
        let travel = track_height - thumb;
        let offset = travel * menu.top_row() as u32 / (total - layout.rows as u32);
        let x = layout.width as i32 - layout.margin / 2;
        self.canvas.set_draw_color(PANEL);
        let _ = self
            .canvas
            .fill_rect(Rect::new(x, layout.grid_y, 4, track_height));
        self.canvas.set_draw_color(MUTED);
        let _ = self
            .canvas
            .fill_rect(Rect::new(x, layout.grid_y + offset as i32, 4, thumb));
    }

    fn draw_header(&mut self, menu: &MenuState, layout: Layout) {
        let y = (layout.height / 40) as i32;
        let summary = (!menu.is_empty()).then(|| {
            format!(
                "{}   |   {} of {}",
                menu.sort().label(),
                menu.selected_position() + 1,
                menu.len()
            )
        });
        // Cover labels are cut to fit, so the full title belongs up here.
        let selected = menu.selected_game().map(|game| game.title.as_str());

        if let Some(banner_width) = self.draw_banner(layout) {
            let right = layout.width - banner_width - 3 * layout.margin as u32;
            if let Some(title) = selected {
                self.draw_text_right(title, FontSize::Heading, TEXT, layout, y, right);
            }
            if let Some(summary) = summary {
                let below = y + (layout.height / 14) as i32;
                self.draw_text_right(&summary, FontSize::Small, MUTED, layout, below, right);
            }
        } else {
            let title = selected.unwrap_or("Games");
            let width = layout.width * 2 / 3;
            self.draw_text(title, FontSize::Heading, TEXT, layout.margin, y, width);
            if let Some(summary) = summary {
                let width = layout.width / 4;
                self.draw_text_right(&summary, FontSize::Small, MUTED, layout, y + 8, width);
            }
        }
    }

    /// Draws the banner if there is one and returns its width.
    fn draw_banner(&mut self, layout: Layout) -> Option<u32> {
        let Some(Some(texture)) = &self.banner else {
            return None;
        };
        let query = texture.query();
        let area = layout.banner_area();
        let fitted = fit_inside(query.width, query.height, area);
        // Left-aligned: fit_inside centres, which would float it mid-header.
        let target = Area {
            x: area.x,
            ..fitted
        };
        let _ = self.canvas.copy(texture, None, rect(target));
        Some(target.width)
    }

    fn draw_footer(&mut self, menu: &MenuState, layout: Layout, controllers: usize) {
        let y = (layout.height - layout.footer_height / 2 - layout.height / 80) as i32;
        let hints = if controllers == 0 {
            "No controller connected"
        } else if menu.is_empty() {
            ""
        } else {
            "A  Play      LB / RB  Page      Y  Sort"
        };
        self.draw_text(
            hints,
            FontSize::Small,
            MUTED,
            layout.margin,
            y,
            layout.width - 2 * layout.margin as u32,
        );
        if let Some(game) = menu.selected_game() {
            let platform = game.platform.to_uppercase();
            let width = layout.width / 4;
            self.draw_text_right(&platform, FontSize::Small, MUTED, layout, y, width);
        }
    }

    fn draw_notice(&mut self, menu: &MenuState, layout: Layout) {
        let Some(notice) = menu.notice() else {
            return;
        };
        let height = layout.footer_height;
        let area = Area {
            x: 0,
            y: (layout.height - height) as i32,
            width: layout.width,
            height,
        };
        self.canvas
            .set_draw_color(if notice.is_error { ERROR } else { INFO });
        let _ = self.canvas.fill_rect(rect(area));
        self.draw_text_centered(&notice.text.clone(), FontSize::Body, TEXT, area);
    }

    fn draw_empty_state(&mut self, menu: &MenuState, layout: Layout) {
        let middle = (layout.height / 2) as i32;
        let area = |y: i32, height: u32| Area {
            x: 0,
            y,
            width: layout.width,
            height,
        };
        let line = layout.height / 16;
        self.draw_text_centered(
            "No games found",
            FontSize::Heading,
            TEXT,
            area(middle - line as i32 * 2, line),
        );
        self.draw_text_centered(
            "Add a folder containing a game.toml file to the games directory, then restart.",
            FontSize::Small,
            MUTED,
            area(middle - line as i32, line),
        );
        if menu.skipped_definitions() > 0 {
            let message = format!(
                "{} game definition(s) could not be read. See the log for details.",
                menu.skipped_definitions()
            );
            self.draw_text_centered(&message, FontSize::Small, ACCENT, area(middle, line));
        }
    }

    /// Loads a PNG. Any failure just means the placeholder is shown.
    fn load_image(&self, path: &Path, max_side: u32) -> Option<Texture<'c>> {
        if !path.is_file() {
            return None;
        }
        let loaded = Surface::from_file(path)
            .and_then(|surface| downscale(surface, max_side))
            .and_then(|surface| {
                self.creator
                    .create_texture_from_surface(&surface)
                    .map_err(|e| e.to_string())
            });
        loaded
            .inspect_err(|e| log_warn!("cannot load image {}: {e}", path.display()))
            .ok()
    }

    /// Renders text, cut with an ellipsis to `max_width`, and returns its
    /// cached texture key.
    fn prepare_text(
        &mut self,
        text: &str,
        size: FontSize,
        color: Color,
        max_width: u32,
    ) -> Option<TextKey> {
        let fonts = self.fonts.as_ref().filter(|_| !text.is_empty())?;
        let key = TextKey {
            text: text.to_string(),
            size,
            color: (color.r, color.g, color.b),
            max_width,
        };
        if !self.text.contains_key(&key) {
            if self.text.len() >= MAX_CACHED_TEXT {
                self.text.clear();
            }
            let font = fonts.get(size);
            let fitted = fit_text(font, text, max_width);
            let surface = font.render(&fitted).blended(color).ok()?;
            let texture = self.creator.create_texture_from_surface(&surface).ok()?;
            self.text.insert(key.clone(), texture);
        }
        Some(key)
    }

    fn blit_text(&mut self, key: &TextKey, place: impl Fn(u32, u32) -> (i32, i32)) {
        if let Some(texture) = self.text.get(key) {
            let query = texture.query();
            let (x, y) = place(query.width, query.height);
            let _ = self
                .canvas
                .copy(texture, None, Rect::new(x, y, query.width, query.height));
        }
    }

    fn draw_text(&mut self, text: &str, size: FontSize, color: Color, x: i32, y: i32, width: u32) {
        if let Some(key) = self.prepare_text(text, size, color, width) {
            self.blit_text(&key, |_, _| (x, y));
        }
    }

    fn draw_text_right(
        &mut self,
        text: &str,
        size: FontSize,
        color: Color,
        layout: Layout,
        y: i32,
        max_width: u32,
    ) {
        let right = layout.width as i32 - layout.margin;
        if let Some(key) = self.prepare_text(text, size, color, max_width) {
            self.blit_text(&key, |width, _| (right - width as i32, y));
        }
    }

    fn draw_text_centered(&mut self, text: &str, size: FontSize, color: Color, area: Area) {
        if let Some(key) = self.prepare_text(text, size, color, area.width) {
            self.blit_text(&key, |width, height| {
                (
                    area.x + (area.width as i32 - width as i32) / 2,
                    area.y + (area.height as i32 - height as i32) / 2,
                )
            });
        }
    }

    /// Drops the least recently drawn covers, keeping what is on screen.
    fn evict(&mut self, menu: &MenuState) {
        let capacity = MIN_CACHED_COVERS.max(menu.visible_positions().len() * 3);
        if self.covers.len() <= capacity {
            return;
        }
        let mut ages: Vec<(u64, String)> = self
            .covers
            .iter()
            .map(|(id, cover)| (cover.last_used, id.clone()))
            .collect();
        ages.sort_unstable();
        for (last_used, id) in ages.into_iter().take(self.covers.len() - capacity) {
            if last_used != self.frame {
                self.covers.remove(&id);
            }
        }
    }
}

fn rect(area: Area) -> Rect {
    Rect::new(area.x, area.y, area.width.max(1), area.height.max(1))
}

fn fit_text(font: &Font<'_, 'static>, text: &str, max_width: u32) -> String {
    let fits = |value: &str| font.size_of(value).is_ok_and(|(w, _)| w <= max_width);
    if fits(text) {
        return text.to_string();
    }
    let mut kept: Vec<char> = text.chars().collect();
    while !kept.is_empty() {
        kept.pop();
        let candidate: String = kept.iter().collect::<String>().trim_end().to_string() + "...";
        if fits(&candidate) {
            return candidate;
        }
    }
    String::from("...")
}

fn downscale(surface: Surface<'static>, max_side: u32) -> Result<Surface<'static>, String> {
    let (width, height) = surface.size();
    let longest = width.max(height);
    if longest <= max_side {
        return Ok(surface);
    }
    let scaled = |side: u32| (side * max_side / longest).max(1);
    let mut source = surface;
    source.set_blend_mode(BlendMode::None)?;
    let mut target = Surface::new(scaled(width), scaled(height), PixelFormatEnum::RGBA32)?;
    source.blit_scaled(None, &mut target, None)?;
    Ok(target)
}
