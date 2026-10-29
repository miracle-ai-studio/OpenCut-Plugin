use super::file_hash;

#[test]
fn standard_vectors_keep_the_runtime_digest_exact() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime");
    for (bytes, expected) in [
        (
            b"".as_slice(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc".as_slice(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".as_slice(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(file_hash(&path).unwrap(), expected);
    }
    std::fs::write(&path, vec![b'a'; 1_000_000]).unwrap();
    assert_eq!(
        file_hash(&path).unwrap(),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn padding_and_read_boundaries_match_independent_file_vectors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime");
    // Independent Python hashlib references, including both sides of SHA-256
    // padding/block boundaries and the unchanged 64 KiB streaming read size.
    for (length, expected) in [
        (
            55,
            "8c5c621bc11584f713ffbee30e5c42f0fcf158b969e9751fae05f9b33793d66c",
        ),
        (
            56,
            "1b347aed0d806623b371b1e752d79cac68afa69295a4c8bb339924328e612d17",
        ),
        (
            63,
            "6e4fee014a455502bee069c994f9c75aaf1131ca7d1dc4faac9800cb3f5e95d4",
        ),
        (
            64,
            "9ac04e0742bc8b69321551829f451b5dbfb0f1e29204a50fa542228ee0dbb04d",
        ),
        (
            65,
            "712a89ef36758f0fe951946906e754778328a760cd45d9e28cc2e434950130de",
        ),
        (
            65_535,
            "23ffddbb9fb4ef2e9e4b49616d65b79c12791da5a980ddbad5caafa6364ae41d",
        ),
        (
            65_536,
            "9b20907c103bdd17580b66531e03ea1da8a8dc243f508b3d70c4c92323c6a383",
        ),
        (
            65_537,
            "1bff264835df1e75b354536d4d7bcaf5ed9e00ad0c3c629cdd1731af8655b575",
        ),
        (
            131_089,
            "7910090ccc263b6d25ab02b6bdbe8229f8c0f4558324c5b9a8c6924fd4fb9572",
        ),
    ] {
        let bytes: Vec<u8> = (0..length)
            .map(|i| ((i * 73 + i / 17 + 19) & 255) as u8)
            .collect();
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(file_hash(&path).unwrap(), expected, "length {length}");
    }
}

#[test]
fn same_size_replacement_is_read_again() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime");
    std::fs::write(&path, b"abc").unwrap();
    assert_eq!(
        file_hash(&path).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    std::fs::write(&path, b"abd").unwrap();
    assert_eq!(
        file_hash(&path).unwrap(),
        "a52d159f262b2c6ddb724a61840befc36eb30c88877a4030b65cbe86298449c9"
    );
}

#[test]
fn unavailable_runtime_keeps_the_original_error_policy() {
    let directory = tempfile::tempdir().unwrap();
    let missing = file_hash(&directory.path().join("missing")).unwrap_err();
    assert_eq!(missing.code, "NATIVE_PROCESS_FAILED");
    assert_eq!(missing.message, "Cannot open native runtime identity");
    #[cfg(unix)]
    {
        let unreadable = file_hash(directory.path()).unwrap_err();
        assert_eq!(unreadable.code, "NATIVE_PROCESS_FAILED");
        assert_eq!(unreadable.message, "Cannot hash native runtime identity");
    }
}
