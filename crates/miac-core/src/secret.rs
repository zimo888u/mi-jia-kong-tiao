//! Owned secrets redact Debug output and clear their allocation on Drop.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);
impl SecretString { pub fn as_str(&self) -> &str { self.0.as_str() } }
impl std::ops::Deref for SecretString {
    type Target = str;
    fn deref(&self) -> &str { self.as_str() }
}
impl From<String> for SecretString {
    fn from(value: String) -> Self { Self(Zeroizing::new(value)) }
}
impl From<&str> for SecretString {
    fn from(value: &str) -> Self { value.to_owned().into() }
}
impl PartialEq<&str> for SecretString {
    fn eq(&self, other: &&str) -> bool { self.as_str() == *other }
}
impl PartialEq<String> for SecretString {
    fn eq(&self, other: &String) -> bool { self.as_str() == other }
}
impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("[redacted]") }
}
impl Serialize for SecretString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn diagnostics_redact_secrets_but_storage_preserves_the_schema() {
        let secret = super::SecretString::from("fake-sensitive-token");
        assert!(!format!("{secret:?}").contains("fake-sensitive-token"));
        let json = serde_json::to_string(&secret).unwrap();
        let restored: super::SecretString = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.as_str(), "fake-sensitive-token");
    }
}
