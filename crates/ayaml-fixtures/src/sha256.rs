//! Minimal, dependency-free FIPS 180-4 SHA-256 implementation for deterministic fixture manifests.

use std::fmt::Write as _;

const INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

const ROUND_CONSTANTS: [u32; 64] = [
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

/// Incremental SHA-256 digest state.
#[derive(Clone, Debug)]
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    total_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// Create a fresh SHA-256 hasher.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: INITIAL_STATE,
            buffer: [0; 64],
            buffer_len: 0,
            total_len: 0,
        }
    }

    /// Absorb `data` into the digest state.
    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self
            .total_len
            .wrapping_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        if self.buffer_len > 0 {
            let take = (64 - self.buffer_len).min(data.len());
            self.buffer[self.buffer_len..self.buffer_len + take].copy_from_slice(&data[..take]);
            self.buffer_len += take;
            data = &data[take..];
            if self.buffer_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffer_len = 0;
            }
        }
        while data.len() >= 64 {
            let mut block = [0_u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffer_len = data.len();
        }
    }

    /// Finalize the digest and return its 32 bytes.
    #[must_use]
    pub fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;
        if self.buffer_len > 56 {
            self.buffer[self.buffer_len..].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffer.fill(0);
        } else {
            self.buffer[self.buffer_len..56].fill(0);
        }
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut digest = [0_u8; 32];
        for (idx, word) in self.state.iter().enumerate() {
            digest[idx * 4..(idx + 1) * 4].copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    /// Finalize the digest as a lowercase 64-character hexadecimal string.
    #[must_use]
    pub fn finalize_hex(self) -> String {
        let digest = self.finalize();
        let mut out = String::with_capacity(64);
        for byte in digest {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut schedule = [0_u32; 64];
        for i in 0..16 {
            schedule[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = schedule[i - 15].rotate_right(7)
                ^ schedule[i - 15].rotate_right(18)
                ^ (schedule[i - 15] >> 3);
            let s1 = schedule[i - 2].rotate_right(17)
                ^ schedule[i - 2].rotate_right(19)
                ^ (schedule[i - 2] >> 10);
            schedule[i] = schedule[i - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[i - 7])
                .wrapping_add(s1);
        }

        let [
            mut w0,
            mut w1,
            mut w2,
            mut w3,
            mut w4,
            mut w5,
            mut w6,
            mut w7,
        ] = self.state;
        for i in 0..64 {
            let s1 = w4.rotate_right(6) ^ w4.rotate_right(11) ^ w4.rotate_right(25);
            let ch = (w4 & w5) ^ ((!w4) & w6);
            let temp1 = w7
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(ROUND_CONSTANTS[i])
                .wrapping_add(schedule[i]);
            let s0 = w0.rotate_right(2) ^ w0.rotate_right(13) ^ w0.rotate_right(22);
            let maj = (w0 & w1) ^ (w0 & w2) ^ (w1 & w2);
            let temp2 = s0.wrapping_add(maj);

            w7 = w6;
            w6 = w5;
            w5 = w4;
            w4 = w3.wrapping_add(temp1);
            w3 = w2;
            w2 = w1;
            w1 = w0;
            w0 = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(w0);
        self.state[1] = self.state[1].wrapping_add(w1);
        self.state[2] = self.state[2].wrapping_add(w2);
        self.state[3] = self.state[3].wrapping_add(w3);
        self.state[4] = self.state[4].wrapping_add(w4);
        self.state[5] = self.state[5].wrapping_add(w5);
        self.state[6] = self.state[6].wrapping_add(w6);
        self.state[7] = self.state[7].wrapping_add(w7);
    }
}

/// Compute the lowercase hex SHA-256 digest of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize_hex()
}
