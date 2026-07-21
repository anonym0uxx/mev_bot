//! Internal macros.
//!
//! ## Responsibility
//! A minimal, dependency-free `bitflags`-style generator so the crate stays
//! hermetic (no external crates) while still expressing manipulation-flag sets
//! as an inspectable bitset. Only the surface this crate needs is generated.

/// Generate a `bitflags`-style newtype with `bits()`, `contains`, `all`,
/// `empty`, `from_bits_truncate`, and bit-or operators.
macro_rules! bitflags_like {
    (
        $(#[$outer:meta])*
        pub struct $Name:ident: $T:ty {
            $(
                $(#[$inner:meta])*
                const $Flag:ident = $value:expr;
            )*
        }
    ) => {
        $(#[$outer])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $Name($T);

        impl $Name {
            $(
                $(#[$inner])*
                pub const $Flag: $Name = $Name($value);
            )*

            /// Empty flag set.
            #[must_use]
            pub const fn empty() -> $Name {
                $Name(0)
            }

            /// Union of every defined flag.
            #[must_use]
            pub const fn all() -> $Name {
                $Name(0 $(| $value)*)
            }

            /// Raw backing bits.
            #[must_use]
            pub const fn bits(self) -> $T {
                self.0
            }

            /// Construct from raw bits, keeping only defined flag positions.
            #[must_use]
            pub const fn from_bits_truncate(bits: $T) -> $Name {
                $Name(bits & $Name::all().0)
            }

            /// Whether `self` contains every bit in `other`.
            #[must_use]
            pub const fn contains(self, other: $Name) -> bool {
                self.0 & other.0 == other.0
            }
        }

        impl core::ops::BitOr for $Name {
            type Output = $Name;
            fn bitor(self, rhs: $Name) -> $Name {
                $Name(self.0 | rhs.0)
            }
        }

        impl core::ops::BitOrAssign for $Name {
            fn bitor_assign(&mut self, rhs: $Name) {
                self.0 |= rhs.0;
            }
        }
    };
}

pub(crate) use bitflags_like;
