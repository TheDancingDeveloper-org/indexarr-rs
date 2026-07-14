use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("identity not initialized")]
    NotInitialized,
    #[error("invalid recovery key: {0}")]
    InvalidRecoveryKey(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("signature error: {0}")]
    Signature(#[from] ed25519_dalek::SignatureError),
}

pub type Result<T> = std::result::Result<T, IdentityError>;

/// Encode 32-byte private key as human-readable recovery key (base32, groups of 4).
fn encode_recovery_key(private_bytes: &[u8; 32]) -> String {
    let encoded = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, private_bytes);
    encoded
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("-")
}

/// Decode a recovery key back to 32 bytes.
fn decode_recovery_key(recovery_key: &str) -> Result<[u8; 32]> {
    let cleaned: String = recovery_key.replace(['-', ' '], "").to_uppercase();
    let bytes = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &cleaned)
        .ok_or_else(|| IdentityError::InvalidRecoveryKey("invalid base32".into()))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| IdentityError::InvalidRecoveryKey("expected 32 bytes".into()))?;
    Ok(arr)
}

/// Manages this node's contributor identity (Ed25519 keypair).
pub struct ContributorIdentity {
    data_dir: PathBuf,
    signing_key: Option<SigningKey>,
    contributor_id: Option<String>,
}

impl ContributorIdentity {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            signing_key: None,
            contributor_id: None,
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.signing_key.is_some()
    }

    pub fn contributor_id(&self) -> Option<&str> {
        self.contributor_id.as_deref()
    }

    pub fn public_key_bytes(&self) -> Option<[u8; 32]> {
        self.signing_key
            .as_ref()
            .map(|sk| sk.verifying_key().to_bytes())
    }

    pub fn public_key_b64(&self) -> Option<String> {
        self.public_key_bytes().map(|b| BASE64.encode(b))
    }

    /// True if the user hasn't acknowledged their recovery key yet.
    pub fn needs_onboarding(&self) -> bool {
        self.pending_file().exists()
    }

    /// Read the pending recovery key, if any.
    pub fn pending_recovery_key(&self) -> Option<String> {
        let path = self.pending_file();
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Mark onboarding as complete.
    pub fn acknowledge_onboarding(&self) {
        let _ = std::fs::remove_file(self.pending_file());
    }

    /// Load existing identity or generate a new one.
    /// Returns (is_new, recovery_key) — recovery_key only set for new identities.
    pub fn load_or_generate(&mut self) -> Result<(bool, Option<String>)> {
        let key_file = self.key_file();
        if key_file.exists() {
            self.load_from_file()?;
            Ok((false, None))
        } else {
            let recovery_key = self.generate_new()?;
            // Write recovery key to pending file for UI display
            std::fs::create_dir_all(&self.data_dir)?;
            std::fs::write(self.pending_file(), &recovery_key)?;
            Ok((true, Some(recovery_key)))
        }
    }

    /// Restore identity from a recovery key.
    pub fn restore_from_recovery_key(&mut self, recovery_key: &str) -> Result<()> {
        let private_bytes = decode_recovery_key(recovery_key)?;
        let signing_key = SigningKey::from_bytes(&private_bytes);
        self.contributor_id = Some(derive_id(&signing_key));
        self.signing_key = Some(signing_key);
        self.save_to_file()?;
        Ok(())
    }

    /// Sign data with the contributor's private key.
    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>> {
        let sk = self
            .signing_key
            .as_ref()
            .ok_or(IdentityError::NotInitialized)?;
        Ok(sk.sign(data).to_bytes().to_vec())
    }

    /// Sign delta record metadata, returns base64 signature.
    pub fn sign_delta_meta(
        &self,
        info_hash: &str,
        name: Option<&str>,
        size: Option<i64>,
        epoch: i32,
    ) -> Result<String> {
        let payload = format!(
            "{}:{}:{}:{}",
            info_hash,
            name.unwrap_or(""),
            size.unwrap_or(0),
            epoch
        );
        let sig = self.sign(payload.as_bytes())?;
        Ok(BASE64.encode(sig))
    }

    fn generate_new(&mut self) -> Result<String> {
        let mut csprng = rand::rand_core::UnwrapErr(rand::rngs::SysRng);
        let signing_key = SigningKey::generate(&mut csprng);
        self.contributor_id = Some(derive_id(&signing_key));

        let private_bytes: [u8; 32] = signing_key.to_bytes();
        let recovery_key = encode_recovery_key(&private_bytes);

        self.signing_key = Some(signing_key);
        self.save_to_file()?;
        Ok(recovery_key)
    }

    fn save_to_file(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        let sk = self
            .signing_key
            .as_ref()
            .ok_or(IdentityError::NotInitialized)?;
        std::fs::write(self.key_file(), sk.to_bytes())?;
        Ok(())
    }

    fn load_from_file(&mut self) -> Result<()> {
        let bytes = std::fs::read(self.key_file())?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| IdentityError::InvalidRecoveryKey("key file not 32 bytes".into()))?;
        let signing_key = SigningKey::from_bytes(&arr);
        self.contributor_id = Some(derive_id(&signing_key));
        self.signing_key = Some(signing_key);
        Ok(())
    }

    fn key_file(&self) -> PathBuf {
        self.data_dir.join("contributor.key")
    }

    fn pending_file(&self) -> PathBuf {
        self.data_dir.join(".recovery_key_pending")
    }
}

/// Derive contributor ID from public key: TN-{sha256(pubkey)[:8]}
fn derive_id(signing_key: &SigningKey) -> String {
    let pub_bytes = signing_key.verifying_key().to_bytes();
    let digest = Sha256::digest(pub_bytes);
    let hex = hex::encode(digest);
    format!("TN-{}", &hex[..8])
}

/// Verify an Ed25519 signature from a contributor.
pub fn verify_signature(public_key_b64: &str, signature_b64: &str, data: &[u8]) -> bool {
    let Ok(pub_bytes) = BASE64.decode(public_key_b64) else {
        return false;
    };
    let Ok(pub_arr): std::result::Result<[u8; 32], _> = pub_bytes.try_into() else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pub_arr) else {
        return false;
    };
    let Ok(sig_bytes) = BASE64.decode(signature_b64) else {
        return false;
    };
    let Ok(sig_arr): std::result::Result<[u8; 64], _> = sig_bytes.try_into() else {
        return false;
    };
    let signature = Signature::from_bytes(&sig_arr);
    verifying_key.verify(data, &signature).is_ok()
}

/// Verify a delta record's contributor signature.
pub fn verify_delta_signature(
    public_key_b64: &str,
    signature_b64: &str,
    info_hash: &str,
    name: Option<&str>,
    size: Option<i64>,
    epoch: i32,
) -> bool {
    let payload = format!(
        "{}:{}:{}:{}",
        info_hash,
        name.unwrap_or(""),
        size.unwrap_or(0),
        epoch
    );
    verify_signature(public_key_b64, signature_b64, payload.as_bytes())
}

// --- Ban List ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BanEntry {
    contributor_id: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BanFile {
    #[serde(default)]
    bans: Vec<BanEntry>,
    #[serde(default)]
    updated_at: f64,
}

pub struct BanList {
    maintainer_pubkey: String,
    banned: HashMap<String, String>,
    ban_file: PathBuf,
}

impl BanList {
    pub fn new(maintainer_pubkey: &str, data_dir: &Path) -> Self {
        Self {
            maintainer_pubkey: maintainer_pubkey.to_string(),
            banned: HashMap::new(),
            ban_file: data_dir.join("bans.json"),
        }
    }

    pub fn is_banned(&self, contributor_id: &str) -> bool {
        self.banned.contains_key(contributor_id)
    }

    pub fn banned_ids(&self) -> &HashMap<String, String> {
        &self.banned
    }

    pub fn load(&mut self) {
        if let Ok(data) = std::fs::read_to_string(&self.ban_file)
            && let Ok(ban_file) = serde_json::from_str::<BanFile>(&data)
        {
            for ban in ban_file.bans {
                self.banned.insert(ban.contributor_id, ban.reason);
            }
        }
    }

    pub fn add_verified_ban(
        &mut self,
        contributor_id: &str,
        reason: &str,
        signature_b64: &str,
    ) -> bool {
        if self.maintainer_pubkey.is_empty() {
            return false;
        }
        let payload = format!("ban:{contributor_id}:{reason}");
        if verify_signature(&self.maintainer_pubkey, signature_b64, payload.as_bytes()) {
            self.banned
                .insert(contributor_id.to_string(), reason.to_string());
            self.save();
            true
        } else {
            false
        }
    }

    fn save(&self) {
        let ban_file = BanFile {
            bans: self
                .banned
                .iter()
                .map(|(cid, reason)| BanEntry {
                    contributor_id: cid.clone(),
                    reason: reason.clone(),
                })
                .collect(),
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        };
        if let Some(parent) = self.ban_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &self.ban_file,
            serde_json::to_string_pretty(&ban_file).unwrap_or_default(),
        );
    }
}

// Re-export hex for convenience
pub use hex;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "indexarr-identity-{label}-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    #[test]
    fn generated_identity_round_trips_recovery_and_signatures() {
        let first_dir = temp_dir("generate");
        let restored_dir = temp_dir("restore");

        let mut generated = ContributorIdentity::new(&first_dir);
        let (is_new, recovery_key) = generated.load_or_generate().unwrap();
        assert!(is_new);
        let recovery_key = recovery_key.unwrap();

        let contributor_id = generated.contributor_id().unwrap().to_owned();
        let public_key = generated.public_key_b64().unwrap();
        let payload = b"dependency-upgrade-regression";
        let signature = BASE64.encode(generated.sign(payload).unwrap());
        assert!(verify_signature(&public_key, &signature, payload));
        assert!(!verify_signature(&public_key, &signature, b"tampered"));

        let mut restored = ContributorIdentity::new(&restored_dir);
        restored.restore_from_recovery_key(&recovery_key).unwrap();
        assert_eq!(restored.contributor_id(), Some(contributor_id.as_str()));
        assert_eq!(
            restored.public_key_b64().as_deref(),
            Some(public_key.as_str())
        );

        let _ = std::fs::remove_dir_all(first_dir);
        let _ = std::fs::remove_dir_all(restored_dir);
    }
}
