// Copyright 2024 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use borsh::BorshSerialize;
use risc0_zkp::core::digest::Digest;
use solana_program::alt_bn128::compression::prelude::{
    alt_bn128_g1_decompress, alt_bn128_g2_decompress,
};
use solana_program::alt_bn128::prelude::{
    alt_bn128_addition, alt_bn128_multiplication, alt_bn128_pairing,
};
use solana_program::entrypoint::ProgramResult;
use solana_program::program_error::ProgramError;

pub enum Risc0SolanaError {
    G1CompressionError,
    G2CompressionError,
    VerificationError,
    InvalidPublicInput,
    SerializationError,
    DeserializationError,
    InvalidInstructionData,
    ArithmeticError,
    PairingError,
}

const G1_LEN: usize = 64;
const G2_LEN: usize = 128;

pub struct Verifier<'a, const N_PUBLIC: usize> {
    proof: &'a Proof,
    public: &'a PublicInputs<N_PUBLIC>,
    vk: &'a VerificationKey<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    pub pi_a: [u8; 64],
    pub pi_b: [u8; 128],
    pub pi_c: [u8; 64],
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize)]
pub struct VerificationKey<'a> {
    pub nr_pubinputs: u32,
    pub vk_alpha_g1: [u8; G1_LEN],
    pub vk_beta_g2: [u8; G2_LEN],
    pub vk_gamma_g2: [u8; G2_LEN],
    pub vk_delta_g2: [u8; G2_LEN],
    pub vk_ic: &'a [[u8; G1_LEN]],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicInputs<const N: usize> {
    pub inputs: [[u8; 32]; N],
}

pub fn decompress_g1(g1_bytes: &[u8; 32]) -> Result<[u8; 64], Risc0SolanaError> {
    alt_bn128_g1_decompress(g1_bytes).map_err(|_| Risc0SolanaError::G1CompressionError)
}

pub fn decompress_g2(g2_bytes: &[u8; 64]) -> Result<[u8; 128], Risc0SolanaError> {
    alt_bn128_g2_decompress(g2_bytes).map_err(|_| Risc0SolanaError::G2CompressionError)
}

impl From<Risc0SolanaError> for ProgramError {
    fn from(error: Risc0SolanaError) -> Self {
        ProgramError::Custom(error as u32)
    }
}

impl<'a, const N_PUBLIC: usize> Verifier<'a, N_PUBLIC> {
    pub fn new(
        proof: &'a Proof,
        public: &'a PublicInputs<N_PUBLIC>,
        vk: &'a VerificationKey<'a>,
    ) -> Self {
        Self { proof, public, vk }
    }

    pub fn verify(&self) -> ProgramResult {
        let prepared_public = self.prepare_public_inputs()?;
        self.perform_pairing(&prepared_public)
    }

    fn prepare_public_inputs(&self) -> Result<[u8; 64], Risc0SolanaError> {
        let mut prepared = self.vk.vk_ic[0];
        for (i, input) in self.public.inputs.iter().enumerate() {
            let mul_res =
                alt_bn128_multiplication(&[&self.vk.vk_ic[i + 1][..], &input[..]].concat())
                    .map_err(|_| Risc0SolanaError::ArithmeticError)?;
            prepared = alt_bn128_addition(&[&mul_res[..], &prepared[..]].concat())
                .unwrap()
                .try_into()
                .map_err(|_| Risc0SolanaError::ArithmeticError)?;
        }
        Ok(prepared)
    }

    fn perform_pairing(&self, prepared_public: &[u8; 64]) -> ProgramResult {
        let pairing_input = [
            self.proof.pi_a.as_slice(),
            self.proof.pi_b.as_slice(),
            prepared_public.as_slice(),
            self.vk.vk_gamma_g2.as_slice(),
            self.proof.pi_c.as_slice(),
            self.vk.vk_delta_g2.as_slice(),
            self.vk.vk_alpha_g1.as_slice(),
            self.vk.vk_beta_g2.as_slice(),
        ]
        .concat();

        let pairing_res =
            alt_bn128_pairing(&pairing_input).map_err(|_| Risc0SolanaError::PairingError)?;

        if pairing_res[31] != 1 {
            return Err(Risc0SolanaError::VerificationError.into());
        }

        Ok(())
    }
}

pub fn public_inputs(
    claim_digest: [u8; 32],
    allowed_control_root: &str,
    bn254_identity_control_id: &str,
) -> Result<PublicInputs<5>, ProgramError> {
    let allowed_control_root: Digest = digest_from_hex(allowed_control_root);
    let bn254_identity_control_id: Digest = digest_from_hex(bn254_identity_control_id);

    let (a0, a1) =
        split_digest_bytes(allowed_control_root).map_err(|_| ProgramError::InvalidAccountData)?;
    let (c0, c1) = split_digest_bytes(Digest::from(claim_digest))
        .map_err(|_| ProgramError::InvalidAccountData)?;

    let mut id_bn554 = bn254_identity_control_id.as_bytes().to_vec();
    id_bn554.reverse();
    let id_bn254_fr = to_fixed_array(&id_bn554);

    let inputs = [a0, a1, c0, c1, id_bn254_fr];

    Ok(PublicInputs { inputs })
}

fn digest_from_hex(hex_str: &str) -> Digest {
    let bytes = hex::decode(hex_str).expect("Invalid hex string");
    Digest::from_bytes(bytes.try_into().expect("Invalid digest length"))
}

fn split_digest_bytes(d: Digest) -> Result<([u8; 32], [u8; 32]), anyhow::Error> {
    let big_endian: Vec<u8> = d.as_bytes().iter().rev().copied().collect();
    let middle = big_endian.len() / 2;
    let (b, a) = big_endian.split_at(middle);
    Ok((to_fixed_array(a), to_fixed_array(b)))
}

fn to_fixed_array(input: &[u8]) -> [u8; 32] {
    assert!(input.len() <= 32, "Input length must not exceed 32 bytes");

    let mut fixed_array = [0u8; 32];
    let start_index = 32 - input.len();

    fixed_array[start_index..].copy_from_slice(input);
    fixed_array
}

#[cfg(not(target_os = "solana"))]
pub mod non_solana {

    use super::*;
    use {
        anyhow::{anyhow, Error, Result},
        ark_serialize::{CanonicalDeserialize, CanonicalSerialize, Compress, Validate},
        num_bigint::BigUint,
        serde::{Deserialize, Deserializer, Serialize},
        solana_program::alt_bn128::compression::prelude::convert_endianness,
        std::{convert::TryInto, fs::File, io::Write},
    };

    type G1 = ark_bn254::g1::G1Affine;
    type G2 = ark_bn254::g2::G2Affine;

    // Base field: q = 21888242871839275222246405745257275088696311157297823662689037894645226208583
    // https://docs.rs/ark-bn254/latest/ark_bn254/
    const BASE_FIELD_MODULUS: &str =
        "21888242871839275222246405745257275088696311157297823662689037894645226208583";

    #[derive(Deserialize, Serialize, Debug, PartialEq)]
    struct ProofJson {
        pi_a: Vec<String>,
        pi_b: Vec<Vec<String>>,
        pi_c: Vec<String>,
        protocol: String,
        curve: String,
    }

    #[derive(Deserialize, Serialize, Debug, PartialEq)]
    struct VerifyingKeyJson {
        protocol: String,
        curve: String,
        #[serde(rename = "nPublic")]
        nr_pubinputs: u32,
        vk_alpha_1: Vec<String>,
        vk_beta_2: Vec<Vec<String>>,
        vk_gamma_2: Vec<Vec<String>>,
        vk_delta_2: Vec<Vec<String>>,
        #[serde(rename = "IC")]
        vk_ic: Vec<Vec<String>>,
    }

    impl<'de> Deserialize<'de> for VerificationKey<'_> {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let json = VerifyingKeyJson::deserialize(deserializer)?;
            VerificationKey::try_from(json).map_err(serde::de::Error::custom)
        }
    }

    impl<'de, const N: usize> Deserialize<'de> for PublicInputs<N> {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let inputs: Vec<String> =
                <Vec<String> as serde::Deserialize>::deserialize(deserializer)?;
            PublicInputs::try_from(inputs).map_err(serde::de::Error::custom)
        }
    }

    impl Serialize for VerificationKey<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let json = self.to_json().map_err(serde::ser::Error::custom)?;
            json.serialize(serializer)
        }
    }

    impl<const N: usize> Serialize for PublicInputs<N> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let strings: Vec<String> = self
                .inputs
                .iter()
                .map(|input| BigUint::from_bytes_be(input).to_string())
                .collect();
            serde::Serialize::serialize(&strings, serializer)
        }
    }

    impl<'a> TryFrom<VerifyingKeyJson> for VerificationKey<'a> {
        type Error = Error;

        fn try_from(json: VerifyingKeyJson) -> Result<Self, Self::Error> {
            let vk_ic: Vec<[u8; G1_LEN]> = json
                .vk_ic
                .iter()
                .map(|ic| convert_g1(ic))
                .collect::<Result<Vec<_>, _>>()?;

            let vk_ic_box = Box::new(vk_ic);
            let vk_ic_ref: &'a [[u8; G1_LEN]] = Box::leak(vk_ic_box);

            Ok(VerificationKey {
                nr_pubinputs: json.nr_pubinputs,
                vk_alpha_g1: convert_g1(&json.vk_alpha_1)?,
                vk_beta_g2: convert_g2(&json.vk_beta_2)?,
                vk_gamma_g2: convert_g2(&json.vk_gamma_2)?,
                vk_delta_g2: convert_g2(&json.vk_delta_2)?,
                vk_ic: vk_ic_ref,
            })
        }
    }

    impl<const N: usize> TryFrom<Vec<String>> for PublicInputs<N> {
        type Error = Error;

        fn try_from(inputs: Vec<String>) -> Result<Self, Self::Error> {
            if inputs.len() != N {
                return Err(anyhow!("Invalid number of public inputs"));
            }

            let parsed_inputs = inputs
                .into_iter()
                .map(|input| {
                    let biguint = BigUint::parse_bytes(input.as_bytes(), 10)
                        .ok_or_else(|| anyhow!("Failed to parse input: {}", input))?;
                    let mut bytes = [0u8; 32];
                    let be_bytes = biguint.to_bytes_be();
                    bytes[32 - be_bytes.len()..].copy_from_slice(&be_bytes);
                    Ok(bytes)
                })
                .collect::<Result<Vec<_>, Error>>()?;

            Ok(PublicInputs {
                inputs: parsed_inputs
                    .try_into()
                    .map_err(|_| anyhow!("Conversion failed"))?,
            })
        }
    }

    impl<'a> VerificationKey<'a> {
        fn to_json(&self) -> Result<VerifyingKeyJson> {
            Ok(VerifyingKeyJson {
                protocol: "groth16".to_string(),
                curve: "bn128".to_string(),
                nr_pubinputs: self.nr_pubinputs,
                vk_alpha_1: export_g1(&self.vk_alpha_g1),
                vk_beta_2: export_g2(&self.vk_beta_g2),
                vk_gamma_2: export_g2(&self.vk_gamma_g2),
                vk_delta_2: export_g2(&self.vk_delta_g2),
                vk_ic: self.vk_ic.iter().map(export_g1).collect(),
            })
        }
    }

    impl<'de> Deserialize<'de> for Proof {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let json = ProofJson::deserialize(deserializer)?;
            Proof::try_from(json).map_err(serde::de::Error::custom)
        }
    }

    impl Serialize for Proof {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let json = self.to_json().map_err(serde::ser::Error::custom)?;
            json.serialize(serializer)
        }
    }

    impl TryFrom<ProofJson> for Proof {
        type Error = Error;

        fn try_from(json: ProofJson) -> Result<Self, Self::Error> {
            Ok(Proof {
                pi_a: convert_g1(&json.pi_a)?,
                pi_b: convert_g2(&json.pi_b)?,
                pi_c: convert_g1(&json.pi_c)?,
            })
        }
    }

    impl Proof {
        fn to_json(&self) -> Result<ProofJson> {
            Ok(ProofJson {
                pi_a: export_g1(&self.pi_a),
                pi_b: export_g2(&self.pi_b),
                pi_c: export_g1(&self.pi_c),
                protocol: "groth16".to_string(),
                curve: "bn128".to_string(),
            })
        }

        pub fn to_bytes(&self) -> [u8; 256] {
            let mut bytes = [0u8; 256];
            bytes[..64].copy_from_slice(&self.pi_a);
            bytes[64..192].copy_from_slice(&self.pi_b);
            bytes[192..].copy_from_slice(&self.pi_c);
            bytes
        }
    }
    fn convert_g1(values: &[String]) -> Result<[u8; G1_LEN]> {
        if values.len() != 3 {
            return Err(anyhow!(
                "Invalid G1 point: expected 3 values, got {}",
                values.len()
            ));
        }

        let x = BigUint::parse_bytes(values[0].as_bytes(), 10)
            .ok_or_else(|| anyhow!("Failed to parse G1 x coordinate"))?;
        let y = BigUint::parse_bytes(values[1].as_bytes(), 10)
            .ok_or_else(|| anyhow!("Failed to parse G1 y coordinate"))?;

        let mut result = [0u8; G1_LEN];
        let x_bytes = x.to_bytes_be();
        let y_bytes = y.to_bytes_be();

        result[32 - x_bytes.len()..32].copy_from_slice(&x_bytes);
        result[G1_LEN - y_bytes.len()..].copy_from_slice(&y_bytes);

        Ok(result)
    }

    fn convert_g2(values: &[Vec<String>]) -> Result<[u8; G2_LEN]> {
        if values.len() != 3 || values[0].len() != 2 || values[1].len() != 2 || values[2].len() != 2
        {
            return Err(anyhow!("Invalid G2 point structure"));
        }

        let x_c0 = BigUint::parse_bytes(values[0][0].as_bytes(), 10)
            .ok_or_else(|| anyhow!("Failed to parse G2 x.c0"))?;
        let x_c1 = BigUint::parse_bytes(values[0][1].as_bytes(), 10)
            .ok_or_else(|| anyhow!("Failed to parse G2 x.c1"))?;
        let y_c0 = BigUint::parse_bytes(values[1][0].as_bytes(), 10)
            .ok_or_else(|| anyhow!("Failed to parse G2 y.c0"))?;
        let y_c1 = BigUint::parse_bytes(values[1][1].as_bytes(), 10)
            .ok_or_else(|| anyhow!("Failed to parse G2 y.c1"))?;

        let mut result = [0u8; G2_LEN];
        let x_c1_bytes = x_c1.to_bytes_be();
        let x_c0_bytes = x_c0.to_bytes_be();
        let y_c1_bytes = y_c1.to_bytes_be();
        let y_c0_bytes = y_c0.to_bytes_be();

        result[32 - x_c1_bytes.len()..32].copy_from_slice(&x_c1_bytes);
        result[64 - x_c0_bytes.len()..64].copy_from_slice(&x_c0_bytes);
        result[96 - y_c1_bytes.len()..96].copy_from_slice(&y_c1_bytes);
        result[G2_LEN - y_c0_bytes.len()..].copy_from_slice(&y_c0_bytes);

        Ok(result)
    }

    fn export_g1(bytes: &[u8; G1_LEN]) -> Vec<String> {
        let x = BigUint::from_bytes_be(&bytes[..32]);
        let y = BigUint::from_bytes_be(&bytes[32..]);
        vec![x.to_string(), y.to_string(), "1".to_string()]
    }

    fn export_g2(bytes: &[u8; G2_LEN]) -> Vec<Vec<String>> {
        let x_c1 = BigUint::from_bytes_be(&bytes[..32]);
        let x_c0 = BigUint::from_bytes_be(&bytes[32..64]);
        let y_c1 = BigUint::from_bytes_be(&bytes[64..96]);
        let y_c0 = BigUint::from_bytes_be(&bytes[96..]);
        vec![
            vec![x_c0.to_string(), x_c1.to_string()],
            vec![y_c0.to_string(), y_c1.to_string()],
            vec!["1".to_string(), "0".to_string()],
        ]
    }

    pub fn write_to_file(filename: &str, proof: &Proof) {
        let mut file = File::create(filename).expect("Failed to create file");
        file.write_all(&proof.pi_a)
            .expect("Failed to write proof_a");
        file.write_all(&proof.pi_b)
            .expect("Failed to write proof_b");
        file.write_all(&proof.pi_c)
            .expect("Failed to write proof_c");
    }

    pub fn write_compressed_proof_to_file(filename: &str, proof: &[u8]) {
        let mut file = File::create(filename).expect("Failed to create file");
        file.write_all(proof).expect("Failed to write proof");
    }

    pub fn compress_g1_be(g1: &[u8; 64]) -> [u8; 32] {
        let g1 = convert_endianness::<32, 64>(g1);
        let mut compressed = [0u8; 32];
        let g1 = G1::deserialize_with_mode(g1.as_slice(), Compress::No, Validate::Yes).unwrap();
        G1::serialize_with_mode(&g1, &mut compressed[..], Compress::Yes).unwrap();
        convert_endianness::<32, 32>(&compressed)
    }

    pub fn compress_g2_be(g2: &[u8; 128]) -> [u8; 64] {
        let g2: [u8; 128] = convert_endianness::<64, 128>(g2);
        let mut compressed = [0u8; 64];
        let g2 = G2::deserialize_with_mode(g2.as_slice(), Compress::No, Validate::Yes).unwrap();
        G2::serialize_with_mode(&g2, &mut compressed[..], Compress::Yes).unwrap();
        convert_endianness::<64, 64>(&compressed)
    }

    pub fn negate_g1(point: &[u8; 64]) -> Result<[u8; 64], Error> {
        let x = &point[..32];
        let y = &point[32..];

        let mut y_big = BigUint::from_bytes_be(y);

        let field_modulus = BigUint::parse_bytes(BASE_FIELD_MODULUS.as_bytes(), 10)
            .ok_or_else(|| anyhow!("Failed to parse field modulus"))?;
        y_big = field_modulus - y_big;

        let mut result = [0u8; 64];
        result[..32].copy_from_slice(x);
        let y_bytes = y_big.to_bytes_be();
        result[64 - y_bytes.len()..].copy_from_slice(&y_bytes);

        Ok(result)
    }
}

#[cfg(test)]
mod test_lib {
    use super::non_solana::*;
    use super::*;
    use risc0_zkvm::sha::Digestible;
    use risc0_zkvm::Receipt;
    use std::fs::File;
    use std::io::Write;

    // From: https://github.com/risc0/risc0/blob/v1.1.2/risc0/circuit/recursion/src/control_id.rs#L47
    // Values updated for risc0 1.1.2
    const ALLOWED_CONTROL_ROOT: &str =
        "8b6dcf11d463ac455361b41fb3ed053febb817491bdea00fdb340e45013b852e";
    const BN254_IDENTITY_CONTROL_ID: &str =
        "4e160df1e119ac0e3d658755a9edf38c8feb307b34bc10b57f4538dbe122a005";

    fn load_receipt_and_extract_data() -> (Receipt, Proof, PublicInputs<5>) {
        let receipt_json_str = include_bytes!("../test/data/receipt.json");
        let receipt: Receipt = serde_json::from_slice(receipt_json_str).unwrap();

        let claim_digest = receipt
            .inner
            .groth16()
            .unwrap()
            .claim
            .digest()
            .try_into()
            .unwrap();
        let public_inputs = public_inputs(
            claim_digest,
            ALLOWED_CONTROL_ROOT,
            BN254_IDENTITY_CONTROL_ID,
        )
        .unwrap();

        let proof_raw = &receipt.inner.groth16().unwrap().seal;
        let mut proof = Proof {
            pi_a: proof_raw[0..64].try_into().unwrap(),
            pi_b: proof_raw[64..192].try_into().unwrap(),
            pi_c: proof_raw[192..256].try_into().unwrap(),
        };
        proof.pi_a = negate_g1(&proof.pi_a).unwrap();

        (receipt, proof, public_inputs)
    }

    fn load_verification_key() -> VerificationKey<'static> {
        let vk_json_str = include_str!("../test/data/r0_test_vk.json");
        serde_json::from_str(vk_json_str).unwrap()
    }

    #[test]
    fn test_import() {
        let vk = load_verification_key();
        println!("Verification Key: {:?}", vk);
    }

    #[test]
    fn test_roundtrip() {
        let vk = load_verification_key();

        let exported_json = serde_json::to_string(&vk).unwrap();
        let reimported_vk: VerificationKey = serde_json::from_str(&exported_json).unwrap();

        assert_eq!(vk, reimported_vk, "Roundtrip serialization failed");
    }

    #[test]
    fn test_public_inputs() {
        let (_, _, public_inputs) = load_receipt_and_extract_data();
        println!("{:?}", public_inputs);

        // Test roundtrip
        let exported_json = serde_json::to_string(&public_inputs).unwrap();
        println!("{:?}", exported_json);
        let reimported_inputs: PublicInputs<5> = serde_json::from_str(&exported_json).unwrap();
        assert_eq!(
            public_inputs, reimported_inputs,
            "Public Inputs roundtrip failed"
        );
    }

    #[test]
    fn test_proof() {
        let (_, proof, _) = load_receipt_and_extract_data();
        println!("{:?}", proof);

        // Convert to bytes
        let proof_bytes = proof.to_bytes();

        println!("PROOF: {:?}", proof_bytes);

        // Check that we have 256 bytes
        assert_eq!(proof_bytes.len(), 256);

        // Test roundtrip
        let exported_json = serde_json::to_string(&proof).unwrap();
        let reimported_proof: Proof = serde_json::from_str(&exported_json).unwrap();
        assert_eq!(proof, reimported_proof, "Proof roundtrip failed");

        println!("Proof bytes: {:?}", proof_bytes);
    }

    #[test]
    pub fn test_verify() {
        let (_, proof, public_inputs) = load_receipt_and_extract_data();
        let vk = load_verification_key();

        let verifier: Verifier<5> = Verifier::new(&proof, &public_inputs, &vk);

        assert!(verifier.verify().is_ok(), "Verification failed");
    }

    #[test]
    fn test_write_compressed_proof_to_file() {
        let (_, proof, _) = load_receipt_and_extract_data();

        let compressed_proof_a = compress_g1_be(&proof.pi_a);
        let compressed_proof_b = compress_g2_be(&proof.pi_b);
        let compressed_proof_c = compress_g1_be(&proof.pi_c);

        let compressed_proof = [
            compressed_proof_a.as_slice(),
            compressed_proof_b.as_slice(),
            compressed_proof_c.as_slice(),
        ]
        .concat();

        write_compressed_proof_to_file("test/data/compressed_proof.bin", &compressed_proof);
    }

    #[test]
    fn write_claim_digest_to_file() {
        let claim_digest = get_claim_digest();

        let output_path = "test/data/claim_digest.bin";

        let mut file = File::create(output_path).expect("Failed to create file");
        file.write_all(&claim_digest)
            .expect("Failed to write claim digest to file");

        println!("Raw claim digest written to {:?}", output_path);

        // Verify the file was written correctly
        let read_digest = std::fs::read(output_path).expect("Failed to read claim digest file");
        assert_eq!(
            claim_digest.to_vec(),
            read_digest,
            "Written and read claim digests do not match"
        );
    }

    fn get_claim_digest() -> [u8; 32] {
        let receipt_json_str = include_bytes!("../test/data/receipt.json");
        let receipt: Receipt = serde_json::from_slice(receipt_json_str).unwrap();
        receipt
            .inner
            .groth16()
            .unwrap()
            .claim
            .digest()
            .try_into()
            .unwrap()
    }
}
