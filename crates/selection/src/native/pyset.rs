//! CPython 3.11's `set` iteration order, for the places where Retool's result depends on the order in which it
//! iterates a set of strings (or tuples of them).
//!
//! Retool runs with `PYTHONHASHSEED=0`, so a string's hash is SipHash-1-3 with a zero key over the string's
//! code points stored at the narrowest width that fits them (Latin-1, UCS-2 or UCS-4). A set's iteration order
//! then follows from the hashes and the insertion and removal history alone. This module reproduces
//! `Objects/setobject.c` (3.11): open addressing with nine linear probes, perturbed probing, dummies on
//! discard, and the growth policy.

const LINEAR_PROBES: usize = 9;
const PERTURB_SHIFT: u32 = 5;
const MINSIZE: usize = 8;

/// A value with CPython's hash.
pub(crate) trait PyHash {
    fn py_hash(&self) -> i64;
}

fn siphash13(data: &[u8]) -> u64 {
    let mut v = [
        0x736f_6d65_7073_6575u64,
        0x646f_7261_6e64_6f6d,
        0x6c79_6765_6e65_7261,
        0x7465_6462_7974_6573,
    ];
    fn round(v: &mut [u64; 4]) {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13);
        v[1] ^= v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16);
        v[3] ^= v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21);
        v[3] ^= v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17);
        v[1] ^= v[2];
        v[2] = v[2].rotate_left(32);
    }
    let (chunks, rest) = data.as_chunks::<8>();
    for chunk in chunks {
        let m = u64::from_le_bytes(*chunk);
        v[3] ^= m;
        round(&mut v);
        v[0] ^= m;
    }
    let mut tail = [0u8; 8];
    tail[..rest.len()].copy_from_slice(rest);
    let b = ((data.len() as u64) << 56) | u64::from_le_bytes(tail);
    v[3] ^= b;
    round(&mut v);
    v[0] ^= b;
    v[2] ^= 0xff;
    for _ in 0..3 {
        round(&mut v);
    }
    (v[0] ^ v[1]) ^ (v[2] ^ v[3])
}

impl PyHash for str {
    fn py_hash(&self) -> i64 {
        if self.is_empty() {
            return 0;
        }
        let max = self.chars().map(u32::from).max().unwrap_or(0);
        let mut bytes = Vec::with_capacity(self.len() * 4);
        for c in self.chars().map(u32::from) {
            if max < 0x100 {
                bytes.push(c as u8);
            } else if max < 0x10000 {
                bytes.extend_from_slice(&(c as u16).to_le_bytes());
            } else {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
        }
        let hash = siphash13(&bytes) as i64;
        if hash == -1 { -2 } else { hash }
    }
}

impl<T: PyHash + ?Sized> PyHash for &T {
    fn py_hash(&self) -> i64 {
        (**self).py_hash()
    }
}

impl PyHash for String {
    fn py_hash(&self) -> i64 {
        self.as_str().py_hash()
    }
}

/// CPython's tuple hash (xxHash-based, 3.8+), from the hashes of the items.
pub(crate) fn tuple_hash(lanes: &[i64]) -> i64 {
    const P1: u64 = 11400714785074694791;
    const P2: u64 = 14029467366897019727;
    const P5: u64 = 2870177450012600261;
    let mut acc = P5;
    for &lane in lanes {
        acc = acc.wrapping_add((lane as u64).wrapping_mul(P2));
        acc = acc.rotate_left(31);
        acc = acc.wrapping_mul(P1);
    }
    acc = acc.wrapping_add((lanes.len() as u64) ^ (P5 ^ 3527539));
    if acc == u64::MAX {
        return 1546275796;
    }
    acc as i64
}

impl<A: PyHash, B: PyHash> PyHash for (A, B) {
    fn py_hash(&self) -> i64 {
        tuple_hash(&[self.0.py_hash(), self.1.py_hash()])
    }
}

impl<T: PyHash> PyHash for Vec<T> {
    /// The hash of a Python tuple of the items.
    fn py_hash(&self) -> i64 {
        tuple_hash(&self.iter().map(PyHash::py_hash).collect::<Vec<_>>())
    }
}

#[derive(Clone, Debug)]
enum Slot<K> {
    Empty,
    Dummy,
    Active(i64, K),
}

/// A set with CPython's iteration order.
#[derive(Clone, Debug)]
pub(crate) struct PySet<K> {
    table: Vec<Slot<K>>,
    fill: usize,
    used: usize,
}

impl<K: PyHash + Eq + Clone> Default for PySet<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: PyHash + Eq + Clone> FromIterator<K> for PySet<K> {
    /// `set(iterable)` from a non-set, or a set comprehension: each item added in turn.
    fn from_iter<I: IntoIterator<Item = K>>(items: I) -> Self {
        let mut set = Self::new();
        for item in items {
            set.add(item);
        }
        set
    }
}

impl<K: PyHash + Eq + Clone> PySet<K> {
    pub fn new() -> Self {
        PySet {
            table: vec![Slot::Empty; MINSIZE],
            fill: 0,
            used: 0,
        }
    }

    fn mask(&self) -> usize {
        self.table.len() - 1
    }

    pub fn len(&self) -> usize {
        self.used
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    fn next_probe(i: usize, perturb: &mut u64, mask: usize) -> usize {
        *perturb >>= PERTURB_SHIFT;
        (i.wrapping_mul(5)
            .wrapping_add(1)
            .wrapping_add(*perturb as usize))
            & mask
    }

    pub fn add(&mut self, key: K) {
        let hash = key.py_hash();
        let mask = self.mask();
        let mut i = (hash as u64 as usize) & mask;
        let mut freeslot: Option<usize> = None;
        let mut perturb = hash as u64;
        loop {
            let probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            for e in i..=i + probes {
                match &self.table[e] {
                    Slot::Empty => {
                        if let Some(free) = freeslot {
                            self.used += 1;
                            self.table[free] = Slot::Active(hash, key);
                            return;
                        }
                        self.fill += 1;
                        self.used += 1;
                        self.table[e] = Slot::Active(hash, key);
                        if self.fill * 5 >= mask * 3 {
                            let used = self.used;
                            self.resize(if used > 50000 { used * 2 } else { used * 4 });
                        }
                        return;
                    }
                    Slot::Active(h, k) if *h == hash && *k == key => return,
                    Slot::Active(..) => {}
                    Slot::Dummy => freeslot = Some(e),
                }
            }
            i = Self::next_probe(i, &mut perturb, mask);
        }
    }

    fn find(&self, key: &K) -> Option<usize> {
        let hash = key.py_hash();
        let mask = self.mask();
        let mut i = (hash as u64 as usize) & mask;
        let mut perturb = hash as u64;
        loop {
            let probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            for e in i..=i + probes {
                match &self.table[e] {
                    Slot::Empty => return None,
                    Slot::Active(h, k) if *h == hash && k == key => return Some(e),
                    _ => {}
                }
            }
            i = Self::next_probe(i, &mut perturb, mask);
        }
    }

    pub fn contains(&self, key: &K) -> bool {
        self.find(key).is_some()
    }

    pub fn discard(&mut self, key: &K) {
        if let Some(e) = self.find(key) {
            self.table[e] = Slot::Dummy;
            self.used -= 1;
        }
    }

    fn insert_clean(table: &mut [Slot<K>], hash: i64, key: K) {
        let mask = table.len() - 1;
        let mut perturb = hash as u64;
        let mut i = (hash as u64 as usize) & mask;
        loop {
            let probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            for slot in &mut table[i..=i + probes] {
                if matches!(slot, Slot::Empty) {
                    *slot = Slot::Active(hash, key);
                    return;
                }
            }
            i = Self::next_probe(i, &mut perturb, mask);
        }
    }

    fn resize(&mut self, minused: usize) {
        let mut newsize = MINSIZE;
        while newsize <= minused {
            newsize <<= 1;
        }
        if newsize == MINSIZE && self.table.len() == MINSIZE && self.fill == self.used {
            return;
        }
        let old = std::mem::replace(&mut self.table, vec![Slot::Empty; newsize]);
        self.fill = self.used;
        for slot in old {
            if let Slot::Active(hash, key) = slot {
                Self::insert_clean(&mut self.table, hash, key);
            }
        }
    }

    /// `self.update(other)` where `other` is a set.
    pub fn merge(&mut self, other: &PySet<K>) {
        if other.used == 0 {
            return;
        }
        if (self.fill + other.used) * 5 >= self.mask() * 3 {
            self.resize((self.used + other.used) * 2);
        }
        if self.fill == 0 && self.mask() == other.mask() && other.fill == other.used {
            self.table = other.table.clone();
            self.fill = other.fill;
            self.used = other.used;
            return;
        }
        if self.fill == 0 {
            self.fill = other.used;
            self.used = other.used;
            for slot in &other.table {
                if let Slot::Active(hash, key) = slot {
                    Self::insert_clean(&mut self.table, *hash, key.clone());
                }
            }
            return;
        }
        for slot in &other.table {
            if let Slot::Active(_, key) = slot {
                self.add(key.clone());
            }
        }
    }

    /// `set(other)` or `other.copy()`, where `other` is a set.
    pub fn copy(&self) -> Self {
        let mut set = Self::new();
        set.merge(self);
        set
    }

    pub fn iter(&self) -> impl Iterator<Item = &K> {
        self.table.iter().filter_map(|s| match s {
            Slot::Active(_, k) => Some(k),
            _ => None,
        })
    }

    pub fn to_vec(&self) -> Vec<K> {
        self.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // The expectations are CPython 3.11's, with `PYTHONHASHSEED=0`.
    #[test]
    fn hashes_are_cpythons_with_a_zero_seed() {
        assert_eq!("".py_hash(), 0);
        assert_eq!("a".py_hash(), 4644417185603328019);
        assert_eq!("(USA)".py_hash(), -1698484988462854323);
        assert_eq!("Zh-Hans é".py_hash(), 1259961465957331612);
        assert_eq!("日本".py_hash(), 6243316497235261705);
        assert_eq!("😀x".py_hash(), -8926728262118538918);
        assert_eq!(("En", "Fr").py_hash(), -31520674030412640);
    }

    #[test]
    fn iteration_follows_cpythons_table() {
        let mut set: PySet<String> = strings(&[
            "(USA)", "(En,Fr)", "(Rev 1)", "(Beta)", "(Proto)", "(Disc 1)", "(Alt)", "(Demo)",
            "(Sample)", "(Unl)",
        ])
        .into_iter()
        .collect();
        let expected = [
            "(Disc 1)", "(Rev 1)", "(Sample)", "(Proto)", "(En,Fr)", "(USA)", "(Beta)", "(Alt)",
            "(Unl)", "(Demo)",
        ];
        assert_eq!(set.to_vec(), strings(&expected));
        set.discard(&"(Beta)".to_owned());
        set.add("(Kiosk)".to_owned());
        set.add("(Beta 2)".to_owned());
        let expected = [
            "(Disc 1)", "(Rev 1)", "(Beta 2)", "(Sample)", "(Proto)", "(En,Fr)", "(Kiosk)",
            "(USA)", "(Alt)", "(Unl)", "(Demo)",
        ];
        assert_eq!(set.to_vec(), strings(&expected));
        assert_eq!(set.copy().to_vec(), strings(&expected));
    }
}
