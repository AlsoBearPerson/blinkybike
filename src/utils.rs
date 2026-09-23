use core::sync::atomic::{AtomicI32, AtomicU16, Ordering::Relaxed};

use fixed::types::{I8F24, U8F8};

pub trait AtomicFixed {
    type Value : fixed::traits::Fixed + defmt::Format;

    fn get(&self) -> Self::Value;
    fn set(&self, v: Self::Value);
}

// Makes wrapper types that act basically as a simple Atomic<F: Fixed>.
// Can't quite implement this as a generic yet,
// due to generic_atomics and const trait fns still being unstable.
macro_rules! atomic_fixed {
    ($name:ident, $fixed:ident, $atomic:ident) => {
        pub struct $name($atomic);
        impl $name {
            pub const fn new(v: $fixed) -> Self {
                Self($atomic::new(v.to_bits()))
            }
        }
        impl AtomicFixed for $name {
            type Value = $fixed;

            fn get(&self) -> $fixed {
                $fixed::from_bits(self.0.load(Relaxed))
            }

            fn set(&self, v: $fixed) {
                self.0.store(v.to_bits(), Relaxed);
            }
        }
    };
}

atomic_fixed!(AtomicU8F8, U8F8, AtomicU16);
atomic_fixed!(AtomicI8F24, I8F24, AtomicI32);
