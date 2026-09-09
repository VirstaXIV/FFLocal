//! Where an asset came from, and what that means for sharing it.
//!
//! Files read from a licensed game install are *protected*: they may only be used locally.
//! Mod files are the modder's own work and may travel between clients, so a character or a
//! world built entirely from mods (or from FFLocal's own content) is *shareable*. Engines fill
//! a [`ContentReport`] for everything they load; the runtime shows it and a future shared
//! world refuses anything protected.

/// Origin of one asset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// Read from the game's own archives (protected, local only).
    Game,
    /// Replaced by a mod pack (shareable).
    Mod(String),
    /// Made in or shipped with FFLocal (shareable).
    Original,
}

impl Provenance {
    pub fn is_protected(&self) -> bool {
        matches!(self, Provenance::Game)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetOrigin {
    /// Engine path of the asset (a game path, a DAT id, ...).
    pub path: String,
    pub provenance: Provenance,
}

/// Every asset something was built from, with its origin.
#[derive(Debug, Clone, Default)]
pub struct ContentReport {
    pub assets: Vec<AssetOrigin>,
    /// Caveats (e.g. "textures are only counted once loaded").
    pub notes: Vec<String>,
}

impl ContentReport {
    pub fn push(&mut self, path: &str, provenance: Provenance) {
        if !self.assets.iter().any(|a| a.path == path) {
            self.assets.push(AssetOrigin {
                path: path.to_string(),
                provenance,
            });
        }
    }

    pub fn extend(&mut self, other: &ContentReport) {
        for a in &other.assets {
            self.push(&a.path, a.provenance.clone());
        }
        self.notes.extend(other.notes.iter().cloned());
    }

    pub fn protected_count(&self) -> usize {
        self.assets.iter().filter(|a| a.provenance.is_protected()).count()
    }

    pub fn mod_count(&self) -> usize {
        self.assets.iter().filter(|a| matches!(a.provenance, Provenance::Mod(_))).count()
    }

    /// Mod packs used, in first-use order.
    pub fn packs(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for a in &self.assets {
            if let Provenance::Mod(p) = &a.provenance
                && !out.contains(p)
            {
                out.push(p.clone());
            }
        }
        out
    }

    /// True when nothing protected is used (may be sent to other players).
    pub fn shareable(&self) -> bool {
        self.protected_count() == 0
    }

    /// The counts alone ("13 modded, 348 game files"), for status lines.
    pub fn short_summary(&self) -> String {
        let protected = self.protected_count();
        let mods = self.mod_count();
        match (mods, protected) {
            (0, p) => format!("{p} game files"),
            (m, 0) => format!("{m} modded files, nothing from the game"),
            (m, p) => format!("{m} modded, {p} game files"),
        }
    }

    /// One-line summary for logs: the counts and the packs used.
    pub fn summary(&self) -> String {
        let protected = self.protected_count();
        let mods = self.mod_count();
        let packs = self.packs();
        let mut s = if protected == 0 {
            format!("shareable: {} assets", self.assets.len())
        } else {
            format!("protected: {protected} game assets")
        };
        if mods > 0 {
            s.push_str(&format!(", {mods} from mods ({})", packs.join(", ")));
        }
        s
    }
}
