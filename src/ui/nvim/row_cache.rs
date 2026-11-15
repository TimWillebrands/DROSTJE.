//! Shaped grid rows retained across frames, keyed by the row's cells and everything else
//! a row's shape depends on.
use super::shaping::RowShape;
use crate::neovim::NvimTheme;
use crate::neovim::grid::{Cell, Highlight};
use gpui_kit::*;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// Everything a row's shape depends on besides its own cells.
pub(super) struct RowCacheKey {
    pub(super) font: Font,
    pub(super) font_size: Pixels,
    pub(super) cell: Size<Pixels>,
    /// Columns a row is shaped up to. The viewport's height never changes a row's shape.
    pub(super) visible_cols: usize,
    pub(super) scale_factor: f32,
    pub(super) cols: usize,
    pub(super) theme: NvimTheme,
    pub(super) defaults: [u32; 3],
    /// Compared per entry: a row is reshaped only when an id it uses changed.
    pub(super) highlights: Arc<HashMap<u64, Highlight>>,
}

impl RowCacheKey {
    /// The columns of a `viewport` a row is shaped up to, including a partly visible one.
    pub(super) fn visible_cols(viewport: Size<Pixels>, cell: Size<Pixels>) -> usize {
        (viewport.width / cell.width).ceil().max(0.) as usize
    }

    /// Whether shapes made under `other` stay valid for every highlight they do not use.
    fn same_layout(&self, other: &Self) -> bool {
        self.font == other.font
            && self.font_size == other.font_size
            && self.cell == other.cell
            && self.visible_cols == other.visible_cols
            && self.scale_factor == other.scale_factor
            && self.cols == other.cols
            && self.theme == other.theme
            && self.defaults == other.defaults
    }
}

/// The highlight ids whose definition differs between two tables, including ids only one
/// of them defines (an undefined id draws with the default highlight).
fn changed_highlights(
    previous: &HashMap<u64, Highlight>,
    next: &HashMap<u64, Highlight>,
) -> HashSet<u64> {
    let mut changed: HashSet<u64> = previous
        .iter()
        .filter(|(id, highlight)| next.get(id) != Some(highlight))
        .map(|(id, _)| *id)
        .collect();
    changed.extend(next.keys().filter(|id| !previous.contains_key(id)));
    changed
}

pub(super) struct CachedRow {
    // Retain the allocation so a recycled pointer cannot hit a stale shape, and so a
    // highlight change can tell which rows use it.
    pub(super) source: Arc<Vec<Cell>>,
    pub(super) shape: Arc<RowShape>,
    pub(super) visible: bool,
}

#[derive(Default)]
pub(super) struct RowCache {
    pub(super) key: Option<RowCacheKey>,
    pub(super) rows: HashMap<usize, CachedRow>,
}

impl RowCache {
    pub(super) fn prepare(
        &mut self,
        key: RowCacheKey,
        rows: &[Arc<Vec<Cell>>],
        mut shape: impl FnMut(&[Cell]) -> RowShape,
    ) -> Vec<Arc<RowShape>> {
        match self.key.as_ref() {
            Some(previous) if previous.same_layout(&key) => {
                // Neovim defines highlights as new attribute combinations appear, and
                // each definition publishes a new table; rows using none of the changed
                // ids keep their shapes.
                if !Arc::ptr_eq(&previous.highlights, &key.highlights) {
                    let changed = changed_highlights(&previous.highlights, &key.highlights);
                    if !changed.is_empty() {
                        self.rows.retain(|_, cached| {
                            !cached
                                .source
                                .iter()
                                .any(|cell| changed.contains(&cell.hl_id))
                        });
                    }
                }
            }
            _ => self.rows = HashMap::new(),
        }
        self.key = Some(key);
        for cached in self.rows.values_mut() {
            cached.visible = false;
        }
        let result = rows
            .iter()
            .map(|row| {
                let cached = self
                    .rows
                    .entry(Arc::as_ptr(row) as usize)
                    .or_insert_with(|| CachedRow {
                        source: Arc::clone(row),
                        shape: Arc::new(shape(row)),
                        visible: false,
                    });
                cached.visible = true;
                Arc::clone(&cached.shape)
            })
            .collect();
        // Scrolled-out rows are not an unbounded history of shaped text.
        self.rows.retain(|_, cached| cached.visible);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{Cell, Highlight, NvimTheme, RowCache, RowCacheKey, RowShape};
    use crate::neovim::grid::GridSnapshot;
    use gpui_kit::{font, px, size};
    use std::cell::{Cell as Counter, RefCell};
    use std::sync::Arc;

    fn key(grid: &GridSnapshot) -> RowCacheKey {
        RowCacheKey {
            font: font("monospace"),
            font_size: px(14.),
            cell: size(px(8.), px(18.)),
            visible_cols: 100,
            scale_factor: 2.,
            cols: grid.cols,
            theme: NvimTheme::default(),
            defaults: [grid.default_fg, grid.default_bg, grid.default_sp],
            highlights: Arc::clone(&grid.highlights),
        }
    }

    #[test]
    fn unchanged_and_scrolled_rows_reuse_shapes_but_evicted_rows_do_not() {
        let a = Arc::new(vec![Cell {
            text: "a".into(),
            hl_id: 0,
        }]);
        let b = Arc::new(vec![Cell {
            text: "b".into(),
            hl_id: 0,
        }]);
        let mut grid = GridSnapshot {
            cols: 1,
            rows: vec![a.clone(), b.clone(), a.clone()],
            ..GridSnapshot::default()
        };
        let mut cache = RowCache::default();
        let calls = Counter::new(0);
        let shape = |_: &[Cell]| {
            calls.set(calls.get() + 1);
            RowShape::default()
        };
        let first = cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(calls.get(), 2);
        assert!(Arc::ptr_eq(&first[0], &first[2]));
        grid.cursor = (1, 0);
        let cursor = cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(calls.get(), 2);
        assert!(Arc::ptr_eq(&first[0], &cursor[0]));
        grid.rows.swap(0, 1);
        let scrolled = cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(calls.get(), 2);
        assert!(Arc::ptr_eq(&first[1], &scrolled[0]));
        Arc::make_mut(&mut grid.rows[1])[0].text = "changed".into();
        cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(calls.get(), 3);
        // Only the visible slice is retained. Returning to an evicted row must
        // shape it again rather than keeping a history of every visited row.
        cache.prepare(key(&grid), &grid.rows[..1], shape);
        cache.prepare(key(&grid), &[a], shape);
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn geometry_font_theme_and_highlights_invalidate_retained_shapes() {
        let grid = GridSnapshot {
            cols: 1,
            rows: vec![Arc::new(vec![Cell::default()])],
            ..GridSnapshot::default()
        };
        let mut cache = RowCache::default();
        let calls = Counter::new(0);
        let shape = |_: &[Cell]| {
            calls.set(calls.get() + 1);
            RowShape::default()
        };
        let changes: &[fn(&mut RowCacheKey)] = &[
            |key| key.font = font("another font"),
            |key| key.font_size = px(15.),
            |key| key.cell.width = px(9.),
            |key| key.cell.height = px(20.),
            |key| key.visible_cols = 50,
            |key| key.scale_factor = 1.,
            |key| key.cols = 2,
            |key| key.theme.foreground = 1,
            |key| key.defaults[0] = 1,
            |key| {
                Arc::make_mut(&mut key.highlights).insert(
                    0,
                    Highlight {
                        bold: true,
                        ..Highlight::default()
                    },
                );
            },
        ];
        for change in changes {
            cache.prepare(key(&grid), &grid.rows, shape);
            let before = calls.get();
            let mut changed = key(&grid);
            change(&mut changed);
            cache.prepare(changed, &grid.rows, shape);
            assert_eq!(calls.get(), before + 1);
        }
    }

    #[test]
    fn viewport_height_and_sub_cell_widths_keep_retained_shapes() {
        let grid = GridSnapshot {
            cols: 1,
            rows: vec![Arc::new(vec![Cell::default()])],
            ..GridSnapshot::default()
        };
        // Columns of an 8px wide cell in a viewport of the given size.
        let sized = |width: f32, height: f32| RowCacheKey {
            visible_cols: RowCacheKey::visible_cols(size(px(width), px(height)), key(&grid).cell), // px: test geometry
            ..key(&grid)
        };
        let mut cache = RowCache::default();
        let calls = Counter::new(0);
        let shape = |_: &[Cell]| {
            calls.set(calls.get() + 1);
            RowShape::default()
        };
        let first = cache.prepare(sized(800., 400.), &grid.rows, shape);
        // A taller or shorter editor, or a width inside the same last column, shapes
        // every row exactly as before.
        for (width, height) in [(800., 120.), (800., 401.5), (793., 400.)] {
            let again = cache.prepare(sized(width, height), &grid.rows, shape);
            assert!(Arc::ptr_eq(&first[0], &again[0]));
        }
        assert_eq!(calls.get(), 1);
        // One more partly visible column reaches cells the old shapes left out.
        cache.prepare(sized(801., 400.), &grid.rows, shape);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn highlight_changes_reshape_only_the_rows_that_use_them() {
        let plain = Arc::new(vec![Cell {
            text: "plain".into(),
            hl_id: 0,
        }]);
        let styled = Arc::new(vec![Cell {
            text: "styled".into(),
            hl_id: 1,
        }]);
        let bold = Highlight {
            bold: true,
            ..Highlight::default()
        };
        let mut grid = GridSnapshot {
            cols: 1,
            rows: vec![plain, styled],
            ..GridSnapshot::default()
        };
        Arc::make_mut(&mut grid.highlights).insert(1, bold.clone());
        let mut cache = RowCache::default();
        let shaped = RefCell::new(Vec::new());
        let shape = |row: &[Cell]| {
            shaped.borrow_mut().push(row[0].text.to_string());
            RowShape::default()
        };
        let first = cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(shaped.take(), ["plain", "styled"]);

        // The same table in a new allocation, as a flush that redefines a highlight
        // identically publishes it, keeps every shape.
        grid.highlights = Arc::new((*grid.highlights).clone());
        let same = cache.prepare(key(&grid), &grid.rows, shape);
        assert!(shaped.take().is_empty());
        assert!(Arc::ptr_eq(&first[0], &same[0]) && Arc::ptr_eq(&first[1], &same[1]));

        // Defining a new id leaves rows that do not use it alone. The cache shares the
        // table, so this publishes a new allocation just like a Neovim flush does.
        Arc::make_mut(&mut grid.highlights).insert(2, bold.clone());
        let added = cache.prepare(key(&grid), &grid.rows, shape);
        assert!(shaped.take().is_empty());
        assert!(Arc::ptr_eq(&first[1], &added[1]));

        // Redefining or dropping an id reshapes exactly the rows that use it.
        Arc::make_mut(&mut grid.highlights).insert(
            1,
            Highlight {
                italic: true,
                ..Highlight::default()
            },
        );
        let redefined = cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(shaped.take(), ["styled"]);
        assert!(Arc::ptr_eq(&first[0], &redefined[0]));
        assert!(!Arc::ptr_eq(&first[1], &redefined[1]));
        Arc::make_mut(&mut grid.highlights).remove(&1);
        cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(shaped.take(), ["styled"]);
        // A row that used an id before it was defined drew it with the default
        // highlight, so defining that id reshapes it too.
        Arc::make_mut(&mut grid.highlights).insert(1, bold);
        cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(shaped.take(), ["styled"]);
    }
}
