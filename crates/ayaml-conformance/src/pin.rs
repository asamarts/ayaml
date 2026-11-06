//! What a strict run checks before it trusts the suite.
//!
//! `REVISION`, beside the suite's `src` directory, records how many files the pinned revision
//! has and a digest of their contents. The digest is what this prints, run from that directory:
//!
//! ```sh
//! find src -type f | LC_ALL=C sort | xargs sha256sum | sha256sum
//! ```

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// Check the tree at `root` against the `REVISION` file beside it.
pub(crate) fn verify(root: &Path) -> Result<(), String> {
    let revision_path = root.join("../REVISION");
    let revision = fs::read_to_string(&revision_path)
        .map_err(|error| format!("read {}: {error}", revision_path.display()))?;
    let want_files = recorded(&revision, "files")?;
    let want_digest = recorded(&revision, "content_sha256")?;

    let mut files = Vec::new();
    list_files(root, "src", &mut files)?;
    files.sort();
    if files.len().to_string() != want_files {
        return Err(format!(
            "{}: {} files, but REVISION records {want_files}",
            root.display(),
            files.len()
        ));
    }
    let digest = tree_digest(root, &files)?;
    if digest != want_digest {
        return Err(format!(
            "{}: content_sha256 is {digest}, but REVISION records {want_digest}",
            root.display()
        ));
    }
    Ok(())
}

/// The value of `key=` in a `REVISION` file.
fn recorded<'a>(revision: &'a str, key: &str) -> Result<&'a str, String> {
    let mut values = revision.lines().filter_map(|line| {
        line.split_once('=')
            .filter(|(name, _)| *name == key)
            .map(|(_, value)| value)
    });
    match (values.next(), values.next()) {
        (Some(value), None) if !value.is_empty() => Ok(value),
        _ => Err(format!("REVISION needs exactly one value for {key}")),
    }
}

/// Add each file under `directory` to `out`, by the path `find src -type f` prints for it.
fn list_files(directory: &Path, shown: &str, out: &mut Vec<String>) -> Result<(), String> {
    let describe = |error: std::io::Error| format!("read {}: {error}", directory.display());
    for entry in fs::read_dir(directory).map_err(describe)? {
        let entry = entry.map_err(describe)?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| format!("{}: a file name isn't UTF-8", directory.display()))?;
        let child = format!("{shown}/{name}");
        if entry.file_type().map_err(describe)?.is_dir() {
            list_files(&entry.path(), &child, out)?;
        } else {
            out.push(child);
        }
    }
    Ok(())
}

/// The digest of the `sha256sum` lines for `files`, whose paths all start with `src/`.
fn tree_digest(root: &Path, files: &[String]) -> Result<String, String> {
    let mut lines = String::new();
    for shown in files {
        let relative = shown.strip_prefix("src/").unwrap_or(shown);
        let path = root.join(relative);
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        writeln!(lines, "{}  {shown}", hex(&sha256(&bytes))).expect("write to String");
    }
    Ok(hex(&sha256(lines.as_bytes())))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("write to String");
    }
    out
}

const ROUND: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// SHA-256, from FIPS 180-4, so that a check of test data adds no dependency.
fn sha256(input: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((input.len() as u64) * 8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut schedule = [0_u32; 64];
        for (word, bytes) in schedule.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for t in 16..64 {
            let (w15, w2) = (schedule[t - 15], schedule[t - 2]);
            let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
            let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
            schedule[t] = schedule[t - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[t - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (round, word) in ROUND.iter().zip(schedule) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let first = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(*round)
                .wrapping_add(word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = s0.wrapping_add(majority);
            (h, g, f, e, d, c, b, a) = (
                g,
                f,
                e,
                d.wrapping_add(first),
                c,
                b,
                a,
                first.wrapping_add(second),
            );
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0_u8; 32];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *chunk = word.to_be_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{hex, recorded, sha256};

    #[test]
    fn sha256_matches_the_published_vectors() {
        let cases: [(&[u8], &str); 4] = [
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            // 56 bytes, so the padding needs a second block.
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                &[b'a'; 1000],
                "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(hex(&sha256(input)), want, "{} bytes", input.len());
        }
    }

    #[test]
    fn a_revision_value_has_to_be_there_exactly_once() {
        assert_eq!(recorded("files=351\nlicense=MIT\n", "files"), Ok("351"));
        assert!(recorded("license=MIT\n", "files").is_err());
        assert!(recorded("files=\n", "files").is_err());
        assert!(recorded("files=1\nfiles=2\n", "files").is_err());
        assert!(recorded("myfiles=1\n", "files").is_err());
    }
}
