//! Derive the public Lumi account lookup id used for administrator bootstrap.

use std::io::{self, IsTerminal, Read};

use anyhow::{bail, Context};
use bip39::{Language, Mnemonic};
use lumi_core::{encode_auth_bytes, DerivedAuthMaterial};
use zeroize::Zeroize;

fn main() -> anyhow::Result<()> {
    if io::stdin().is_terminal() {
        bail!("refusing to echo a recovery phrase; pipe hidden input or a protected file to stdin");
    }

    let mut phrase = String::new();
    io::stdin()
        .read_to_string(&mut phrase)
        .context("failed to read recovery phrase from stdin")?;
    let result = derive_lookup_id(&phrase);
    phrase.zeroize();
    println!("{}", encode_auth_bytes(&result?));
    Ok(())
}

fn derive_lookup_id(phrase: &str) -> anyhow::Result<[u8; 32]> {
    let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase.trim())
        .context("recovery phrase is not a valid English BIP39 mnemonic")?;
    if mnemonic.word_count() != 24 {
        bail!("recovery phrase must contain exactly 24 words");
    }
    let mut entropy: [u8; 32] = mnemonic
        .to_entropy()
        .try_into()
        .map_err(|_| anyhow::anyhow!("recovery phrase must encode 256 bits"))?;
    let result = DerivedAuthMaterial::derive(&entropy)
        .map(|material| material.lookup_id())
        .context("failed to derive Lumi authentication material");
    entropy.zeroize();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_same_lookup_id_as_the_shared_auth_contract(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mnemonic = Mnemonic::from_entropy_in(Language::English, &[7; 32])?;
        let expected = DerivedAuthMaterial::derive(&[7; 32])?.lookup_id();

        let actual = derive_lookup_id(&mnemonic.to_string())?;

        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn rejects_a_shorter_mnemonic() -> Result<(), Box<dyn std::error::Error>> {
        let mnemonic = Mnemonic::from_entropy_in(Language::English, &[7; 16])?;

        assert!(derive_lookup_id(&mnemonic.to_string()).is_err());
        Ok(())
    }
}
