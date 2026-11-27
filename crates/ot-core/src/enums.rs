//! Enum (de)serialization matching System.Text.Json's `JsonStringEnumConverter` as OpenTaskbar uses it:
//! values are written by name, and read case-insensitively by name, as an integer, or as a numeric string.
//! `[Flags]` enums are written in .NET's `Enum.ToString()` format ("Cpu, Memory") and read from either form.

/// A plain C# enum persisted by name.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $($variant:ident = $value:expr),+ $(,)? }
        default = $default:ident
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl Default for $name {
            fn default() -> Self { Self::$default }
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => stringify!($variant)),+ }
            }

            pub const fn value(self) -> i64 {
                match self { $(Self::$variant => $value),+ }
            }

            pub fn from_name(text: &str) -> Option<Self> {
                let text = text.trim();
                $(if text.eq_ignore_ascii_case(stringify!($variant)) { return Some(Self::$variant); })+
                text.parse::<i64>().ok().and_then(Self::from_value)
            }

            pub fn from_value(value: i64) -> Option<Self> {
                $(if value == $value { return Some(Self::$variant); })+
                None
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.name())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Visitor;
                impl ::serde::de::Visitor<'_> for Visitor {
                    type Value = $name;
                    fn expecting(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                        write!(f, "a {} name or value", stringify!($name))
                    }
                    fn visit_str<E: ::serde::de::Error>(self, v: &str) -> Result<$name, E> {
                        $name::from_name(v).ok_or_else(|| E::custom(format!("unknown {} '{}'", stringify!($name), v)))
                    }
                    fn visit_i64<E: ::serde::de::Error>(self, v: i64) -> Result<$name, E> {
                        $name::from_value(v).ok_or_else(|| E::custom(format!("unknown {} value {}", stringify!($name), v)))
                    }
                    fn visit_u64<E: ::serde::de::Error>(self, v: u64) -> Result<$name, E> {
                        i64::try_from(v).map_err(E::custom).and_then(|v| self.visit_i64(v))
                    }
                }
                deserializer.deserialize_any(Visitor)
            }
        }
    };
}

/// A C# `[Flags]` enum persisted in `Enum.ToString()` format.
macro_rules! flags_enum {
    (
        $(#[$meta:meta])*
        pub struct $name:ident { $($flag:ident = $value:expr),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
        pub struct $name(pub u32);

        #[allow(non_upper_case_globals)]
        impl $name {
            $(pub const $flag: Self = Self($value);)+

            /// Declared names; flags must be declared in ascending value order, as .NET sorts them
            /// (checked by a unit test).
            pub const NAMES: &'static [(&'static str, u32)] = &[$((stringify!($flag), $value)),+];

            pub const fn bits(self) -> u32 { self.0 }
            pub const fn contains(self, other: Self) -> bool { self.0 & other.0 == other.0 }

            /// .NET `Enum.ToString()` for a `[Flags]` enum.
            pub fn format(self) -> String {
                if let Some((name, _)) = Self::NAMES.iter().find(|(_, v)| *v == self.0) {
                    return (*name).to_owned();
                }
                let mut remaining = self.0;
                let mut parts = Vec::new();
                for (name, value) in Self::NAMES.iter().rev() {
                    if *value != 0 && remaining & value == *value {
                        remaining -= value;
                        parts.push(*name);
                    }
                }
                if remaining != 0 || parts.is_empty() {
                    return self.0.to_string();
                }
                parts.reverse();
                parts.join(", ")
            }

            pub fn parse(text: &str) -> Option<Self> {
                let text = text.trim();
                if let Ok(number) = text.parse::<i64>() {
                    return u32::try_from(number).ok().map(Self);
                }
                let mut bits = 0;
                for token in text.split(',') {
                    let token = token.trim();
                    let (_, value) = Self::NAMES.iter().find(|(name, _)| token.eq_ignore_ascii_case(name))?;
                    bits |= value;
                }
                Some(Self(bits))
            }
        }

        impl ::std::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
        }

        impl ::std::ops::BitAnd for $name {
            type Output = Self;
            fn bitand(self, rhs: Self) -> Self { Self(self.0 & rhs.0) }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.format())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Visitor;
                impl ::serde::de::Visitor<'_> for Visitor {
                    type Value = $name;
                    fn expecting(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                        write!(f, "{} flag names or a number", stringify!($name))
                    }
                    fn visit_str<E: ::serde::de::Error>(self, v: &str) -> Result<$name, E> {
                        $name::parse(v).ok_or_else(|| E::custom(format!("unknown {} '{}'", stringify!($name), v)))
                    }
                    fn visit_i64<E: ::serde::de::Error>(self, v: i64) -> Result<$name, E> {
                        u32::try_from(v).map($name).map_err(E::custom)
                    }
                    fn visit_u64<E: ::serde::de::Error>(self, v: u64) -> Result<$name, E> {
                        u32::try_from(v).map($name).map_err(E::custom)
                    }
                }
                deserializer.deserialize_any(Visitor)
            }
        }
    };
}
