//! Content hashes of named files, through the public API (issue #90).

use assay_domain::ContentHash;
use proptest::collection::vec;
use proptest::prelude::*;

/// Named files as owned data, so properties can edit them.
type Files = Vec<(String, Vec<u8>)>;

fn hash(files: &Files) -> ContentHash {
    ContentHash::of_files(
        files
            .iter()
            .map(|(name, content)| (name.as_str(), content.as_slice())),
    )
}

#[test]
fn hash_of_two_files_matches_the_length_prefixed_sha256_computed_independently() {
    let files = [
        ("v1/system.md", b"shared".as_slice()),
        ("v1/combined.md", b""),
    ];

    let hash = ContentHash::of_files(files);

    // Python: sha256 over, per file, `>Q` length + name, then `>Q` length + content.
    assert_eq!(
        hash.to_string(),
        "sha256:5b4caf7899cf66015523397cce9582ab2a91e8b5c4344f9ecc66340a052588c2"
    );
}

#[test]
fn hash_of_no_files_is_the_sha256_of_nothing() {
    let hash = ContentHash::of_files([]);

    assert_eq!(
        hash.to_string(),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn bytes_are_the_digest_shown_in_hex() {
    let hash = ContentHash::of_files([]);

    let bytes = hash.as_bytes();

    // sha256("") = e3b0c442…7852b855
    assert_eq!(
        (bytes.first_chunk(), bytes.last_chunk()),
        (
            Some(&[0xe3, 0xb0, 0xc4, 0x42]),
            Some(&[0x78, 0x52, 0xb8, 0x55])
        )
    );
}

fn files() -> impl Strategy<Value = Files> {
    vec(("[a-z/.]{0,12}", vec(any::<u8>(), 0..64)), 1..5)
}

proptest! {
    #[test]
    fn changing_any_content_byte_changes_the_hash(
        mut files in files(),
        pick in any::<prop::sample::Index>(),
        at in any::<prop::sample::Index>(),
        flip in 1_u8..,
    ) {
        let before = hash(&files);
        let file = pick.get_mut(&mut files);
        prop_assume!(!file.1.is_empty());
        let byte = at.get_mut(&mut file.1);
        *byte ^= flip;

        prop_assert_ne!(hash(&files), before);
    }

    #[test]
    fn changing_any_name_changes_the_hash(
        mut files in files(),
        pick in any::<prop::sample::Index>(),
        suffix in "[a-z]{1,4}",
    ) {
        let before = hash(&files);
        pick.get_mut(&mut files).0.push_str(&suffix);

        prop_assert_ne!(hash(&files), before);
    }

    #[test]
    fn moving_a_byte_from_one_file_to_the_next_changes_the_hash(
        first in vec(any::<u8>(), 1..32),
        second in vec(any::<u8>(), 0..32),
    ) {
        let (kept, moved) = first.split_at(first.len() - 1);
        let mut shifted = moved.to_vec();
        shifted.extend_from_slice(&second);

        let original = ContentHash::of_files([("a", first.as_slice()), ("b", second.as_slice())]);
        let edited = ContentHash::of_files([("a", kept), ("b", shifted.as_slice())]);

        prop_assert_ne!(edited, original);
    }

    #[test]
    fn moving_a_byte_from_a_name_into_its_content_changes_the_hash(
        name in "[a-z]{1,12}",
        content in vec(any::<u8>(), 0..32),
    ) {
        let (kept, moved) = name.split_at(name.len() - 1);
        let mut shifted = moved.as_bytes().to_vec();
        shifted.extend_from_slice(&content);

        let original = ContentHash::of_files([(name.as_str(), content.as_slice())]);
        let edited = ContentHash::of_files([(kept, shifted.as_slice())]);

        prop_assert_ne!(edited, original);
    }
}
