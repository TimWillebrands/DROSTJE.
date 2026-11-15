use std::{cell::RefCell, rc::Rc};

pub(super) struct Memo<K, V>(RefCell<Option<(K, Rc<V>)>>);

impl<K, V> Default for Memo<K, V> {
    fn default() -> Self {
        Self(RefCell::new(None))
    }
}

impl<K: PartialEq, V> Memo<K, V> {
    pub(super) fn get(&self, key: K, derive: impl FnOnce() -> V) -> Rc<V> {
        let mut slot = self.0.borrow_mut();
        if let Some((_, value)) = slot.as_ref().filter(|(current, _)| *current == key) {
            return value.clone();
        }
        let value = Rc::new(derive());
        *slot = Some((key, value.clone()));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::Memo;
    use std::cell::Cell;

    #[test]
    fn memo_derives_once_per_key() {
        let memo = Memo::<(u8, u64), String>::default();
        let derived = Cell::new(0);
        let derive = |text: &str| {
            derived.set(derived.get() + 1);
            text.to_owned()
        };
        let first = memo.get((1, 7), || derive("a"));
        let again = memo.get((1, 7), || derive("b"));
        assert!(std::rc::Rc::ptr_eq(&first, &again), "same key, same value");
        assert_eq!((derived.get(), again.as_str()), (1, "a"));
        assert_eq!(memo.get((1, 8), || derive("c")).as_str(), "c");
        assert_eq!(memo.get((2, 8), || derive("d")).as_str(), "d");
        assert_eq!(derived.get(), 3);
    }
}
