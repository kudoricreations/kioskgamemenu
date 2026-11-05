//! Menu state and grid geometry. Free of SDL so it can be tested headlessly.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Range;

use serde::Deserialize;

use crate::game::Game;
use crate::statistics::GameStats;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortMode {
    #[default]
    NameAsc,
    NameDesc,
    MostPlayed,
    RecentlyPlayed,
}

impl SortMode {
    pub fn next(self) -> Self {
        match self {
            Self::NameAsc => Self::NameDesc,
            Self::NameDesc => Self::MostPlayed,
            Self::MostPlayed => Self::RecentlyPlayed,
            Self::RecentlyPlayed => Self::NameAsc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NameAsc => "Name A-Z",
            Self::NameDesc => "Name Z-A",
            Self::MostPlayed => "Most played",
            Self::RecentlyPlayed => "Recently played",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
}

/// A message shown over the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub is_error: bool,
}

/// Returns game indices in display order. Ties always fall back to the title
/// and then the unique id, so the order is fully deterministic.
pub fn sort_games(
    games: &[Game],
    stats: &HashMap<String, GameStats>,
    mode: SortMode,
) -> Vec<usize> {
    let keys: Vec<String> = games.iter().map(|g| g.title.to_lowercase()).collect();
    let stats_of = |index: usize| stats.get(&games[index].id).copied().unwrap_or_default();
    let by_name = |a: usize, b: usize| keys[a].cmp(&keys[b]).then_with(|| by_id(games, a, b));

    let mut order: Vec<usize> = (0..games.len()).collect();
    order.sort_unstable_by(|&a, &b| {
        let (sa, sb) = (stats_of(a), stats_of(b));
        match mode {
            SortMode::NameAsc => by_name(a, b),
            SortMode::NameDesc => keys[b].cmp(&keys[a]).then_with(|| by_id(games, a, b)),
            SortMode::MostPlayed => sb
                .launch_count
                .cmp(&sa.launch_count)
                .then_with(|| sb.last_launched.cmp(&sa.last_launched))
                .then_with(|| by_name(a, b)),
            // `None` orders below every timestamp, so unplayed games come last.
            SortMode::RecentlyPlayed => sb
                .last_launched
                .cmp(&sa.last_launched)
                .then_with(|| sb.launch_count.cmp(&sa.launch_count))
                .then_with(|| by_name(a, b)),
        }
    });
    order
}

fn by_id(games: &[Game], a: usize, b: usize) -> Ordering {
    games[a].id.cmp(&games[b].id)
}

pub struct MenuState {
    games: Vec<Game>,
    stats: HashMap<String, GameStats>,
    sort: SortMode,
    /// Indices into `games` in display order.
    order: Vec<usize>,
    /// Selected position within `order`.
    selected: usize,
    columns: usize,
    rows: usize,
    top_row: usize,
    notice: Option<Notice>,
    skipped_definitions: usize,
}

impl MenuState {
    pub fn new(
        games: Vec<Game>,
        stats: HashMap<String, GameStats>,
        sort: SortMode,
        skipped_definitions: usize,
    ) -> Self {
        let order = sort_games(&games, &stats, sort);
        let mut menu = Self {
            games,
            stats,
            sort,
            order,
            selected: 0,
            columns: 1,
            rows: 1,
            top_row: 0,
            notice: None,
            skipped_definitions,
        };
        menu.select_most_recent();
        menu
    }

    /// Starts on the last launched game, which also returns the player to
    /// their game after an `exec` launch restarts the launcher.
    fn select_most_recent(&mut self) {
        let recent = (0..self.order.len())
            .filter(|&position| self.stats_at(position).last_launched.is_some())
            .max_by_key(|&position| self.stats_at(position).last_launched);
        if let Some(position) = recent {
            self.selected = position;
        }
    }

    fn stats_at(&self, position: usize) -> GameStats {
        let id = &self.games[self.order[position]].id;
        self.stats.get(id).copied().unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn sort(&self) -> SortMode {
        self.sort
    }

    pub fn selected_position(&self) -> usize {
        self.selected
    }

    pub fn selected_game(&self) -> Option<&Game> {
        self.game_at(self.selected)
    }

    pub fn game_at(&self, position: usize) -> Option<&Game> {
        self.order.get(position).map(|&index| &self.games[index])
    }

    pub fn top_row(&self) -> usize {
        self.top_row
    }

    pub fn total_rows(&self) -> usize {
        self.len().div_ceil(self.columns)
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    pub fn skipped_definitions(&self) -> usize {
        self.skipped_definitions
    }

    pub fn set_notice(&mut self, text: impl Into<String>, is_error: bool) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error,
        });
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// Applies the grid size, keeping the selected game visible.
    pub fn set_viewport(&mut self, columns: usize, rows: usize) {
        self.columns = columns.max(1);
        self.rows = rows.max(1);
        self.scroll_to_selection();
    }

    /// Positions currently on screen; only these need artwork.
    pub fn visible_positions(&self) -> Range<usize> {
        let start = self.top_row * self.columns;
        let end = (start + self.rows * self.columns).min(self.len());
        start.min(end)..end
    }

    /// Moves the selection, returning whether it changed.
    pub fn navigate(&mut self, nav: Nav) -> bool {
        let Some(last) = self.len().checked_sub(1) else {
            return false;
        };
        let current = self.selected;
        let page = self.columns * self.rows;
        self.selected = match nav {
            Nav::Left => current.saturating_sub(1),
            Nav::Right => (current + 1).min(last),
            Nav::Up if current >= self.columns => current - self.columns,
            Nav::Up => current,
            Nav::Down if current + self.columns <= last => current + self.columns,
            // On the row above a short last row, drop to its final game.
            Nav::Down if current / self.columns < last / self.columns => last,
            Nav::Down => current,
            Nav::PageUp => current.saturating_sub(page),
            Nav::PageDown => (current + page).min(last),
        };
        self.scroll_to_selection();
        self.selected != current
    }

    /// Advances the sort mode, keeping the same game selected.
    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.resort();
    }

    /// Updates in-memory statistics so sorting reflects a launch immediately,
    /// even if the database could not be written.
    pub fn note_launch(&mut self, id: &str, timestamp: i64) {
        let stats = self.stats.entry(id.to_string()).or_default();
        stats.launch_count += 1;
        stats.last_launched = Some(timestamp);
        self.resort();
    }

    fn resort(&mut self) {
        let selected = self.order.get(self.selected).copied();
        self.order = sort_games(&self.games, &self.stats, self.sort);
        if let Some(position) = selected.and_then(|s| self.order.iter().position(|&i| i == s)) {
            self.selected = position;
        }
        self.scroll_to_selection();
    }

    fn scroll_to_selection(&mut self) {
        let row = self.selected / self.columns;
        if row < self.top_row {
            self.top_row = row;
        } else if row >= self.top_row + self.rows {
            self.top_row = row + 1 - self.rows;
        }
        self.top_row = self
            .top_row
            .min(self.total_rows().saturating_sub(self.rows));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Screen geometry, recomputed whenever the output size changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub columns: usize,
    pub rows: usize,
    pub margin: i32,
    pub gap: i32,
    pub grid_x: i32,
    pub grid_y: i32,
    pub header_height: u32,
    pub footer_height: u32,
    pub cover_width: u32,
    pub cover_height: u32,
    pub title_height: u32,
}

impl Layout {
    pub fn compute(width: u32, height: u32) -> Self {
        let (width, height) = (width.max(160), height.max(120));
        let margin = (width / 40).max(8);
        let gap = (height / 40).max(6);
        let header_height = height / 10;
        let footer_height = height / 12;
        let title_height = height / 18;

        let grid_width = width - 2 * margin;
        let grid_height = height - header_height - footer_height;
        let rows = (height / 300).clamp(1, 3);
        let cell_height = (grid_height - (rows - 1) * gap) / rows;
        let cover_height = cell_height.saturating_sub(title_height).max(1);
        let cover_width = (cover_height * 3 / 4).clamp(1, grid_width);
        let columns = ((grid_width + gap) / (cover_width + gap)).max(1);
        let used_width = columns * cover_width + (columns - 1) * gap;

        Self {
            width,
            height,
            columns: columns as usize,
            rows: rows as usize,
            margin: margin as i32,
            gap: gap as i32,
            grid_x: ((width - used_width) / 2) as i32,
            grid_y: header_height as i32,
            header_height,
            footer_height,
            cover_width,
            cover_height,
            title_height,
        }
    }

    /// The cover area of a slot, counted from the top-left visible cell.
    pub fn cover_area(&self, slot: usize) -> Area {
        let column = (slot % self.columns) as i32;
        let row = (slot / self.columns) as i32;
        Area {
            x: self.grid_x + column * (self.cover_width as i32 + self.gap),
            y: self.grid_y + row * ((self.cover_height + self.title_height) as i32 + self.gap),
            width: self.cover_width,
            height: self.cover_height,
        }
    }
}

/// Fits an image inside `area`, preserving aspect ratio and centring it.
pub fn fit_inside(image_width: u32, image_height: u32, area: Area) -> Area {
    let (image_width, image_height) = (image_width.max(1) as u64, image_height.max(1) as u64);
    let (area_width, area_height) = (area.width as u64, area.height as u64);
    let (width, height) = if image_width * area_height > image_height * area_width {
        (area_width, image_height * area_width / image_width)
    } else {
        (image_width * area_height / image_height, area_height)
    };
    Area {
        x: area.x + (area_width - width) as i32 / 2,
        y: area.y + (area_height - height) as i32 / 2,
        width: width as u32,
        height: height as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::test_game;

    fn games(titles: &[(&str, &str)]) -> Vec<Game> {
        titles.iter().map(|(id, t)| test_game(id, t)).collect()
    }

    fn played(entries: &[(&str, u32, Option<i64>)]) -> HashMap<String, GameStats> {
        entries
            .iter()
            .map(|&(id, launch_count, last_launched)| {
                let stats = GameStats {
                    launch_count,
                    last_launched,
                };
                (id.to_string(), stats)
            })
            .collect()
    }

    fn ids(games: &[Game], order: &[usize]) -> Vec<String> {
        order.iter().map(|&i| games[i].id.clone()).collect()
    }

    fn numbered_menu(count: usize, columns: usize, rows: usize) -> MenuState {
        let games = (0..count)
            .map(|i| test_game(&format!("g{i:03}"), &format!("Game {i:03}")))
            .collect();
        let mut menu = MenuState::new(games, HashMap::new(), SortMode::NameAsc, 0);
        menu.set_viewport(columns, rows);
        menu
    }

    #[test]
    fn sorts_by_name_ignoring_case() {
        let list = games(&[("1", "banana"), ("2", "Apple"), ("3", "cherry")]);
        let stats = HashMap::new();
        let ascending = sort_games(&list, &stats, SortMode::NameAsc);
        assert_eq!(ids(&list, &ascending), ["2", "1", "3"]);
        let descending = sort_games(&list, &stats, SortMode::NameDesc);
        assert_eq!(ids(&list, &descending), ["3", "1", "2"]);
    }

    #[test]
    fn identical_titles_order_by_id_in_every_mode() {
        let list = games(&[("b", "Same"), ("a", "Same"), ("c", "Same")]);
        let stats = HashMap::new();
        for mode in [
            SortMode::NameAsc,
            SortMode::NameDesc,
            SortMode::MostPlayed,
            SortMode::RecentlyPlayed,
        ] {
            assert_eq!(
                ids(&list, &sort_games(&list, &stats, mode)),
                ["a", "b", "c"]
            );
        }
    }

    #[test]
    fn most_played_breaks_ties_by_recency_then_name() {
        let list = games(&[
            ("few", "Few"),
            ("old", "Old"),
            ("new", "New"),
            ("zero-b", "Zero B"),
            ("zero-a", "Zero A"),
        ]);
        let stats = played(&[
            ("few", 1, Some(999)),
            ("old", 5, Some(100)),
            ("new", 5, Some(200)),
        ]);
        let order = sort_games(&list, &stats, SortMode::MostPlayed);
        assert_eq!(
            ids(&list, &order),
            ["new", "old", "few", "zero-a", "zero-b"]
        );
    }

    #[test]
    fn recently_played_puts_unplayed_games_last() {
        let list = games(&[("never", "A Never"), ("old", "Old"), ("new", "New")]);
        let stats = played(&[("old", 9, Some(100)), ("new", 1, Some(200))]);
        let order = sort_games(&list, &stats, SortMode::RecentlyPlayed);
        assert_eq!(ids(&list, &order), ["new", "old", "never"]);
    }

    #[test]
    fn sort_cycle_visits_all_modes() {
        let mut mode = SortMode::NameAsc;
        let mut labels = std::collections::HashSet::new();
        for _ in 0..4 {
            labels.insert(mode.label());
            mode = mode.next();
        }
        assert_eq!(labels.len(), 4);
        assert_eq!(mode, SortMode::NameAsc);
    }

    #[test]
    fn changing_sort_keeps_the_selected_game() {
        let list = games(&[("a", "A"), ("b", "B"), ("c", "C")]);
        let mut menu = MenuState::new(list, HashMap::new(), SortMode::NameAsc, 0);
        menu.set_viewport(3, 1);
        menu.navigate(Nav::Right);
        assert_eq!(menu.selected_game().unwrap().id, "b");
        menu.cycle_sort();
        assert_eq!(menu.sort(), SortMode::NameDesc);
        assert_eq!(menu.selected_game().unwrap().id, "b");
        menu.navigate(Nav::Left);
        assert_eq!(menu.selected_game().unwrap().id, "c");
        menu.cycle_sort();
        assert_eq!(menu.selected_game().unwrap().id, "c");
    }

    #[test]
    fn starts_on_the_most_recently_launched_game() {
        let list = games(&[("a", "A"), ("b", "B"), ("c", "C")]);
        let stats = played(&[("a", 1, Some(10)), ("c", 1, Some(20))]);
        let menu = MenuState::new(list, stats, SortMode::NameAsc, 0);
        assert_eq!(menu.selected_game().unwrap().id, "c");
    }

    #[test]
    fn note_launch_resorts_and_keeps_selection() {
        let list = games(&[("a", "A"), ("b", "B")]);
        let mut menu = MenuState::new(list, HashMap::new(), SortMode::MostPlayed, 0);
        menu.set_viewport(2, 1);
        menu.navigate(Nav::Right);
        menu.note_launch("b", 50);
        assert_eq!(menu.selected_position(), 0);
        assert_eq!(menu.selected_game().unwrap().id, "b");
    }

    #[test]
    fn grid_navigation() {
        let mut menu = numbered_menu(10, 4, 2);
        assert!(!menu.navigate(Nav::Left));
        assert!(!menu.navigate(Nav::Up));
        assert!(menu.navigate(Nav::Right));
        assert_eq!(menu.selected_position(), 1);
        menu.navigate(Nav::Down);
        assert_eq!(menu.selected_position(), 5);
        menu.navigate(Nav::Up);
        assert_eq!(menu.selected_position(), 1);
    }

    #[test]
    fn down_reaches_a_short_last_row() {
        let mut menu = numbered_menu(10, 4, 3);
        for _ in 0..7 {
            menu.navigate(Nav::Right);
        }
        assert_eq!(menu.selected_position(), 7);
        assert!(menu.navigate(Nav::Down));
        assert_eq!(menu.selected_position(), 9);
        assert!(!menu.navigate(Nav::Down));
        assert!(!menu.navigate(Nav::Right));
    }

    #[test]
    fn paging_clamps_and_scrolls() {
        let mut menu = numbered_menu(100, 5, 2);
        menu.navigate(Nav::PageDown);
        assert_eq!(menu.selected_position(), 10);
        assert_eq!(menu.top_row(), 1);
        for _ in 0..50 {
            menu.navigate(Nav::PageDown);
        }
        assert_eq!(menu.selected_position(), 99);
        assert_eq!(menu.visible_positions(), 90..100);
        for _ in 0..50 {
            menu.navigate(Nav::PageUp);
        }
        assert_eq!(menu.selected_position(), 0);
        assert_eq!(menu.visible_positions(), 0..10);
    }

    #[test]
    fn large_library_only_exposes_visible_entries() {
        let menu = numbered_menu(500, 6, 3);
        assert_eq!(menu.visible_positions().len(), 18);
    }

    #[test]
    fn viewport_change_keeps_selection_visible() {
        let mut menu = numbered_menu(40, 5, 2);
        for _ in 0..30 {
            menu.navigate(Nav::Right);
        }
        menu.set_viewport(8, 3);
        assert!(menu.visible_positions().contains(&30));
        menu.set_viewport(2, 1);
        assert!(menu.visible_positions().contains(&30));
    }

    #[test]
    fn empty_menu_is_safe() {
        let mut menu = numbered_menu(0, 4, 2);
        assert!(menu.is_empty());
        assert!(menu.selected_game().is_none());
        assert!(!menu.navigate(Nav::PageDown));
        menu.cycle_sort();
        assert_eq!(menu.visible_positions(), 0..0);
    }

    #[test]
    fn layouts_fit_on_screen_at_common_resolutions() {
        for (width, height) in [
            (320, 240),
            (640, 480),
            (800, 600),
            (1280, 720),
            (1920, 1080),
            (2560, 1080),
            (3840, 2160),
            (1080, 1920),
        ] {
            let layout = Layout::compute(width, height);
            let last = layout.cover_area(layout.columns * layout.rows - 1);
            let right = last.x + last.width as i32;
            let bottom = last.y + (last.height + layout.title_height) as i32;
            assert!(layout.columns >= 1 && layout.rows >= 1);
            assert!(right <= width as i32, "{width}x{height}: right {right}");
            assert!(
                bottom <= (height - layout.footer_height) as i32,
                "{width}x{height}: bottom {bottom}"
            );
        }
    }

    #[test]
    fn degenerate_output_size_does_not_panic() {
        let layout = Layout::compute(0, 0);
        assert!(layout.columns >= 1 && layout.cover_width >= 1);
    }

    #[test]
    fn images_fit_without_distortion() {
        let area = Area {
            x: 10,
            y: 20,
            width: 300,
            height: 400,
        };
        let wide = fit_inside(600, 300, area);
        assert_eq!((wide.width, wide.height), (300, 150));
        assert_eq!((wide.x, wide.y), (10, 145));
        let tall = fit_inside(100, 800, area);
        assert_eq!((tall.width, tall.height), (50, 400));
        assert_eq!(fit_inside(300, 400, area), area);
    }
}
