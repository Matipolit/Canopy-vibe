//! [`Shared`]: a value that copies of the project share until one of them
//! changes it.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A value (a clip's notes, an automation lane's points) shared between
/// copies of the project until one of them changes it. Undo keeps whole
/// copies of the project, and most edits touch one clip or lane, so the
/// rest stays shared between all of them instead of being copied with
/// every step.
///
/// It reads as the value it holds; borrowing it mutably copies the value
/// first if another copy of the project still shares it. It is saved and
/// loaded as the value alone, so the file format doesn't see it.
#[derive(Clone, Default)]
pub struct Shared<T: Clone>(Arc<T>);

impl<T: Clone> Shared<T> {
    pub fn new(value: T) -> Shared<T> {
        Shared(Arc::new(value))
    }

    /// Whether `a` and `b` are the same shared value (so certainly equal).
    pub fn ptr_eq(a: &Shared<T>, b: &Shared<T>) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }
}

impl<T: Clone> Deref for Shared<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> DerefMut for Shared<T> {
    /// Copies the value first if anything else shares it.
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}

impl<T: Clone> From<T> for Shared<T> {
    fn from(value: T) -> Shared<T> {
        Shared::new(value)
    }
}

impl<A, T: Clone + FromIterator<A>> FromIterator<A> for Shared<T> {
    fn from_iter<I: IntoIterator<Item = A>>(iter: I) -> Shared<T> {
        Shared::new(iter.into_iter().collect())
    }
}

impl<T: Clone + PartialEq> PartialEq for Shared<T> {
    fn eq(&self, other: &Shared<T>) -> bool {
        Shared::ptr_eq(self, other) || *self.0 == *other.0
    }
}

// Compared with plain lists as the list it holds.
impl<A: Clone + PartialEq<B>, B> PartialEq<Vec<B>> for Shared<Vec<A>> {
    fn eq(&self, other: &Vec<B>) -> bool {
        *self.0 == *other
    }
}

impl<A: Clone + PartialEq<B>, B, const N: usize> PartialEq<[B; N]> for Shared<Vec<A>> {
    fn eq(&self, other: &[B; N]) -> bool {
        *self.0 == *other
    }
}

impl<A: Clone + PartialEq<B>, B> PartialEq<[B]> for Shared<Vec<A>> {
    fn eq(&self, other: &[B]) -> bool {
        *self.0 == *other
    }
}

impl<A: Clone, B: PartialEq<A>> PartialEq<Shared<Vec<A>>> for Vec<B> {
    fn eq(&self, other: &Shared<Vec<A>>) -> bool {
        *self == *other.0
    }
}

impl<T: Clone + fmt::Debug> fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: Clone + Serialize> Serialize for Shared<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Clone + Deserialize<'de>> Deserialize<'de> for Shared<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Shared<T>, D::Error> {
        T::deserialize(deserializer).map(Shared::new)
    }
}

impl<'a, T: Clone> IntoIterator for &'a Shared<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        (**self).into_iter()
    }
}

impl<'a, T: Clone> IntoIterator for &'a mut Shared<T>
where
    &'a mut T: IntoIterator,
{
    type Item = <&'a mut T as IntoIterator>::Item;
    type IntoIter = <&'a mut T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        (**self).into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_share_until_one_changes() {
        let a: Shared<Vec<u32>> = vec![1, 2, 3].into();
        let mut b = a.clone();
        assert!(Shared::ptr_eq(&a, &b) && a == b);
        b.push(4);
        assert!(!Shared::ptr_eq(&a, &b));
        assert_eq!(*a, vec![1, 2, 3], "the original is untouched");
        assert_eq!(*b, vec![1, 2, 3, 4]);
        let c: Shared<Vec<u32>> = vec![1, 2, 3].into();
        assert_eq!(a, c, "equal by value too");
        for x in &mut b {
            *x *= 2;
        }
        assert_eq!(b.iter().sum::<u32>(), 20);
    }

    #[test]
    fn saves_as_the_value_alone() {
        let shared: Shared<Vec<u32>> = vec![5, 6].into();
        let text = ron::to_string(&shared).unwrap();
        assert_eq!(text, ron::to_string(&vec![5u32, 6]).unwrap());
        let back: Shared<Vec<u32>> = ron::from_str(&text).unwrap();
        assert_eq!(back, shared);
    }
}
