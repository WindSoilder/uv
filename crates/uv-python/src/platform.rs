use crate::cpuinfo::detect_hardware_floating_point_support;
use crate::libc::{LibcDetectionError, LibcVersion, detect_linux_libc};
use std::fmt::Display;
use std::ops::Deref;
use std::{fmt, str::FromStr};
use thiserror::Error;

use uv_static::EnvVars;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Unknown operating system: {0}")]
    UnknownOs(String),
    #[error("Unknown architecture: {0}")]
    UnknownArch(String),
    #[error("Unknown libc environment: {0}")]
    UnknownLibc(String),
    #[error("Unsupported variant `{0}` for architecture `{1}`")]
    UnsupportedVariant(String, String),
    #[error(transparent)]
    LibcDetectionError(#[from] LibcDetectionError),
}

/// Architecture variants, e.g., with support for different instruction sets
#[derive(Debug, Eq, PartialEq, Clone, Copy, Hash, Ord, PartialOrd)]
pub enum ArchVariant {
    /// Targets 64-bit Intel/AMD CPUs newer than Nehalem (2008).
    /// Includes SSE3, SSE4 and other post-2003 CPU instructions.
    V2,
    /// Targets 64-bit Intel/AMD CPUs newer than Haswell (2013) and Excavator (2015).
    /// Includes AVX, AVX2, MOVBE and other newer CPU instructions.
    V3,
    /// Targets 64-bit Intel/AMD CPUs with AVX-512 instructions (post-2017 Intel CPUs).
    /// Many post-2017 Intel CPUs do not support AVX-512.
    V4,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Hash)]
pub struct Arch {
    pub(crate) family: target_lexicon::Architecture,
    pub(crate) variant: Option<ArchVariant>,
}

impl Ord for Arch {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self.family == other.family {
            return self.variant.cmp(&other.variant);
        }

        // For the time being, manually make aarch64 windows disfavored
        // on its own host platform, because most packages don't have wheels for
        // aarch64 windows, making emulation more useful than native execution!
        //
        // The reason we do this in "sorting" and not "supports" is so that we don't
        // *refuse* to use an aarch64 windows pythons if they happen to be installed
        // and nothing else is available.
        //
        // Similarly if someone manually requests an aarch64 windows install, we
        // should respect that request (this is the way users should "override"
        // this behaviour).
        let preferred = if cfg!(all(windows, target_arch = "aarch64")) {
            Arch {
                family: target_lexicon::Architecture::X86_64,
                variant: None,
            }
        } else {
            // Prefer native architectures
            Arch::from_env()
        };

        match (
            self.family == preferred.family,
            other.family == preferred.family,
        ) {
            (true, true) => unreachable!(),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => {
                // Both non-preferred, fallback to lexicographic order
                self.family.to_string().cmp(&other.family.to_string())
            }
        }
    }
}

impl PartialOrd for Arch {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Hash)]
pub struct Os(pub(crate) target_lexicon::OperatingSystem);

#[derive(Debug, Eq, PartialEq, Clone, Copy, Hash)]
pub enum Libc {
    Some(target_lexicon::Environment),
    None,
}

impl Libc {
    pub(crate) fn from_env() -> Result<Self, Error> {
        match std::env::consts::OS {
            "linux" => {
                if let Ok(libc) = std::env::var(EnvVars::UV_LIBC) {
                    if !libc.is_empty() {
                        return Self::from_str(&libc);
                    }
                }

                Ok(Self::Some(match detect_linux_libc()? {
                    LibcVersion::Manylinux { .. } => match std::env::consts::ARCH {
                        // Checks if the CPU supports hardware floating-point operations.
                        // Depending on the result, it selects either the `gnueabihf` (hard-float) or `gnueabi` (soft-float) environment.
                        // download-metadata.json only includes armv7.
                        "arm" | "armv5te" | "armv7" => {
                            match detect_hardware_floating_point_support() {
                                Ok(true) => target_lexicon::Environment::Gnueabihf,
                                Ok(false) => target_lexicon::Environment::Gnueabi,
                                Err(_) => target_lexicon::Environment::Gnu,
                            }
                        }
                        _ => target_lexicon::Environment::Gnu,
                    },
                    LibcVersion::Musllinux { .. } => target_lexicon::Environment::Musl,
                }))
            }
            "windows" | "macos" => Ok(Self::None),
            // Use `None` on platforms without explicit support.
            _ => Ok(Self::None),
        }
    }

    pub fn is_musl(&self) -> bool {
        matches!(self, Self::Some(target_lexicon::Environment::Musl))
    }
}

impl FromStr for Libc {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "gnu" => Ok(Self::Some(target_lexicon::Environment::Gnu)),
            "gnueabi" => Ok(Self::Some(target_lexicon::Environment::Gnueabi)),
            "gnueabihf" => Ok(Self::Some(target_lexicon::Environment::Gnueabihf)),
            "musl" => Ok(Self::Some(target_lexicon::Environment::Musl)),
            "none" => Ok(Self::None),
            _ => Err(Error::UnknownLibc(s.to_string())),
        }
    }
}

impl Os {
    pub fn from_env() -> Self {
        Self(target_lexicon::HOST.operating_system)
    }
}

impl Arch {
    pub fn from_env() -> Self {
        Self {
            family: target_lexicon::HOST.architecture,
            variant: None,
        }
    }

    /// Does the current architecture support running the other?
    ///
    /// When the architecture is equal, this is always true. Otherwise, this is true if the
    /// architecture is transparently emulated or is a microarchitecture with worse performance
    /// characteristics.
    pub(crate) fn supports(self, other: Self) -> bool {
        if self == other {
            return true;
        }

        // TODO: Implement `variant` support checks

        // Windows ARM64 runs emulated x86_64 binaries transparently
        // Similarly, macOS aarch64 runs emulated x86_64 binaries transparently if you have Rosetta
        // installed. We don't try to be clever and check if that's the case here, we just assume
        // that if x86_64 distributions are available, they're usable.
        if (cfg!(windows) || cfg!(target_os = "macos"))
            && matches!(self.family, target_lexicon::Architecture::Aarch64(_))
        {
            return other.family == target_lexicon::Architecture::X86_64;
        }

        false
    }

    pub fn family(&self) -> target_lexicon::Architecture {
        self.family
    }

    pub fn is_arm(&self) -> bool {
        matches!(self.family, target_lexicon::Architecture::Arm(_))
    }
}

impl Display for Libc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Some(env) => write!(f, "{env}"),
            Self::None => write!(f, "none"),
        }
    }
}

impl Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &**self {
            target_lexicon::OperatingSystem::Darwin(_) => write!(f, "macos"),
            inner => write!(f, "{inner}"),
        }
    }
}

impl Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.family {
            target_lexicon::Architecture::X86_32(target_lexicon::X86_32Architecture::I686) => {
                write!(f, "x86")?;
            }
            inner => write!(f, "{inner}")?,
        }
        if let Some(variant) = self.variant {
            write!(f, "_{variant}")?;
        }
        Ok(())
    }
}

impl FromStr for Os {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let inner = match s {
            "macos" => target_lexicon::OperatingSystem::Darwin(None),
            _ => target_lexicon::OperatingSystem::from_str(s)
                .map_err(|()| Error::UnknownOs(s.to_string()))?,
        };
        if matches!(inner, target_lexicon::OperatingSystem::Unknown) {
            return Err(Error::UnknownOs(s.to_string()));
        }
        Ok(Self(inner))
    }
}

impl FromStr for Arch {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        fn parse_family(s: &str) -> Result<target_lexicon::Architecture, Error> {
            let inner = match s {
                // Allow users to specify "x86" as a shorthand for the "i686" variant, they should not need
                // to specify the exact architecture and this variant is what we have downloads for.
                "x86" => {
                    target_lexicon::Architecture::X86_32(target_lexicon::X86_32Architecture::I686)
                }
                _ => target_lexicon::Architecture::from_str(s)
                    .map_err(|()| Error::UnknownArch(s.to_string()))?,
            };
            if matches!(inner, target_lexicon::Architecture::Unknown) {
                return Err(Error::UnknownArch(s.to_string()));
            }
            Ok(inner)
        }

        // First check for a variant
        if let Some((Ok(family), Ok(variant))) = s
            .rsplit_once('_')
            .map(|(family, variant)| (parse_family(family), ArchVariant::from_str(variant)))
        {
            // We only support variants for `x86_64` right now
            if !matches!(family, target_lexicon::Architecture::X86_64) {
                return Err(Error::UnsupportedVariant(
                    variant.to_string(),
                    family.to_string(),
                ));
            }
            return Ok(Self {
                family,
                variant: Some(variant),
            });
        }

        let family = parse_family(s)?;

        Ok(Self {
            family,
            variant: None,
        })
    }
}

impl FromStr for ArchVariant {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "v2" => Ok(Self::V2),
            "v3" => Ok(Self::V3),
            "v4" => Ok(Self::V4),
            _ => Err(()),
        }
    }
}

impl Display for ArchVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V2 => write!(f, "v2"),
            Self::V3 => write!(f, "v3"),
            Self::V4 => write!(f, "v4"),
        }
    }
}

impl Deref for Os {
    type Target = target_lexicon::OperatingSystem;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<&uv_platform_tags::Arch> for Arch {
    fn from(value: &uv_platform_tags::Arch) -> Self {
        match value {
            uv_platform_tags::Arch::Aarch64 => Self {
                family: target_lexicon::Architecture::Aarch64(
                    target_lexicon::Aarch64Architecture::Aarch64,
                ),
                variant: None,
            },
            uv_platform_tags::Arch::Armv5TEL => Self {
                family: target_lexicon::Architecture::Arm(target_lexicon::ArmArchitecture::Armv5te),
                variant: None,
            },
            uv_platform_tags::Arch::Armv6L => Self {
                family: target_lexicon::Architecture::Arm(target_lexicon::ArmArchitecture::Armv6),
                variant: None,
            },
            uv_platform_tags::Arch::Armv7L => Self {
                family: target_lexicon::Architecture::Arm(target_lexicon::ArmArchitecture::Armv7),
                variant: None,
            },
            uv_platform_tags::Arch::S390X => Self {
                family: target_lexicon::Architecture::S390x,
                variant: None,
            },
            uv_platform_tags::Arch::Powerpc => Self {
                family: target_lexicon::Architecture::Powerpc,
                variant: None,
            },
            uv_platform_tags::Arch::Powerpc64 => Self {
                family: target_lexicon::Architecture::Powerpc64,
                variant: None,
            },
            uv_platform_tags::Arch::Powerpc64Le => Self {
                family: target_lexicon::Architecture::Powerpc64le,
                variant: None,
            },
            uv_platform_tags::Arch::X86 => Self {
                family: target_lexicon::Architecture::X86_32(
                    target_lexicon::X86_32Architecture::I686,
                ),
                variant: None,
            },
            uv_platform_tags::Arch::X86_64 => Self {
                family: target_lexicon::Architecture::X86_64,
                variant: None,
            },
            uv_platform_tags::Arch::LoongArch64 => Self {
                family: target_lexicon::Architecture::LoongArch64,
                variant: None,
            },
            uv_platform_tags::Arch::Riscv64 => Self {
                family: target_lexicon::Architecture::Riscv64(
                    target_lexicon::Riscv64Architecture::Riscv64,
                ),
                variant: None,
            },
            uv_platform_tags::Arch::Wasm32 => Self {
                family: target_lexicon::Architecture::Wasm32,
                variant: None,
            },
        }
    }
}

impl From<&uv_platform_tags::Os> for Libc {
    fn from(value: &uv_platform_tags::Os) -> Self {
        match value {
            uv_platform_tags::Os::Manylinux { .. } => Self::Some(target_lexicon::Environment::Gnu),
            uv_platform_tags::Os::Musllinux { .. } => Self::Some(target_lexicon::Environment::Musl),
            _ => Self::None,
        }
    }
}

impl From<&uv_platform_tags::Os> for Os {
    fn from(value: &uv_platform_tags::Os) -> Self {
        match value {
            uv_platform_tags::Os::Dragonfly { .. } => {
                Self(target_lexicon::OperatingSystem::Dragonfly)
            }
            uv_platform_tags::Os::FreeBsd { .. } => Self(target_lexicon::OperatingSystem::Freebsd),
            uv_platform_tags::Os::Haiku { .. } => Self(target_lexicon::OperatingSystem::Haiku),
            uv_platform_tags::Os::Illumos { .. } => Self(target_lexicon::OperatingSystem::Illumos),
            uv_platform_tags::Os::Macos { .. } => {
                Self(target_lexicon::OperatingSystem::Darwin(None))
            }
            uv_platform_tags::Os::Manylinux { .. }
            | uv_platform_tags::Os::Musllinux { .. }
            | uv_platform_tags::Os::Android { .. } => Self(target_lexicon::OperatingSystem::Linux),
            uv_platform_tags::Os::NetBsd { .. } => Self(target_lexicon::OperatingSystem::Netbsd),
            uv_platform_tags::Os::OpenBsd { .. } => Self(target_lexicon::OperatingSystem::Openbsd),
            uv_platform_tags::Os::Windows => Self(target_lexicon::OperatingSystem::Windows),
            uv_platform_tags::Os::Pyodide { .. } => {
                Self(target_lexicon::OperatingSystem::Emscripten)
            }
        }
    }
}
