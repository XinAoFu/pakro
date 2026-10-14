use std::fmt::{Display, Formatter};

#[derive(
    Debug,
    PartialEq,
    Eq,
    Hash,
    Clone,
    strum::EnumString,
    strum::EnumIter,
    strum::AsRefStr,
    Ord,
    PartialOrd,
)]
#[strum(serialize_all = "snake_case")]
pub enum BackendType {
    Aqua,
    Asdf,
    Cargo,
    Conda,
    Core,
    Dotnet,
    Forgejo,
    Gem,
    Github,
    Gitlab,
    Go,
    Packslip,
    Spinel,
    Spm,
    Http,
    S3,
    Ubi,
    Unknown,
}

impl Display for BackendType {
    fn fmt(&self, formatter: &mut Formatter) -> std::fmt::Result {
        write!(formatter, "{}", format!("{self:?}").to_lowercase())
    }
}

impl BackendType {
    pub(crate) fn disable_key(&self) -> Option<&str> {
        match self {
            BackendType::Unknown => None,
            _ => Some(self.as_ref()),
        }
    }

    pub(crate) fn guess(s: &str) -> BackendType {
        let prefix = s.split(':').next().unwrap_or(s);

        match prefix {
            "aqua" => BackendType::Aqua,
            "asdf" => BackendType::Asdf,
            "cargo" => BackendType::Cargo,
            "conda" => BackendType::Conda,
            "core" => BackendType::Core,
            "dotnet" => BackendType::Dotnet,
            "forgejo" => BackendType::Forgejo,
            "gem" => BackendType::Gem,
            "github" => BackendType::Github,
            "gitlab" => BackendType::Gitlab,
            "go" => BackendType::Go,
            "packslip" => BackendType::Packslip,
            "spinel" => BackendType::Spinel,
            "spm" => BackendType::Spm,
            "http" => BackendType::Http,
            "s3" => BackendType::S3,
            "ubi" => BackendType::Ubi,
            _ => BackendType::Unknown,
        }
    }

    /// Returns true if this backend is still gated behind experimental mode.
    pub fn is_experimental(&self) -> bool {
        use super::{dotnet, packslip, s3, spinel, spm};
        match self {
            BackendType::Dotnet => dotnet::EXPERIMENTAL,
            BackendType::Packslip => packslip::EXPERIMENTAL,
            BackendType::S3 => s3::EXPERIMENTAL,
            BackendType::Spinel => spinel::EXPERIMENTAL,
            BackendType::Spm => spm::EXPERIMENTAL,
            _ => false,
        }
    }
}
