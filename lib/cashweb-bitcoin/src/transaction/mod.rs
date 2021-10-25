//! This module contains the primary structs related to Bitcoin transactions.
//! All of them enjoy [`Encodable`] and [`Decodable`].

pub mod input;
pub mod outpoint;
pub mod output;
pub mod script;

use std::convert::TryInto;

use bytes::{Buf, BufMut};
use ring::digest::{digest, SHA256};
use thiserror::Error;

use crate::{
    merkle,
    var_int::{DecodeError as VarIntDecodeError, VarInt},
    Decodable, Encodable,
};
#[doc(inline)]
pub use input::{DecodeError as InputDecodeError, Input};
#[doc(inline)]
pub use output::{DecodeError as OutputDecodeError, Output};
#[doc(inline)]
pub use script::Script;

/// Represents a transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct Transaction {
    pub version: u32,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    pub lock_time: u32,
}

/// Enumerates the different signature hash types.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum SignatureHashType {
    All = 0x01,
    None = 0x02,
    Single = 0x03,
    AnyoneCanPayAll = 0x81,
    AnyoneCanPayNone = 0x82,
    AnyoneCanPaySingle = 0x83,
}

impl SignatureHashType {
    /// Checks whether the signature hash is `anyone-can-pay`.
    #[inline]
    pub fn is_anyone_can_pay(&self) -> bool {
        matches!(self, Self::All | Self::None | Self::Single)
    }
}

/// Calculate the transaction hash. This is the double SHA256 digest of the raw transaction in big-endian encoding.
#[inline]
pub fn transaction_hash_rev(raw_transaction: &[u8]) -> [u8; 32] {
    let mut tx_id_le = transaction_hash(raw_transaction);
    tx_id_le.reverse();
    tx_id_le
}

/// Calculate the transaction ID in little-endian format. This is the double SHA256 digest of the raw transaction.
///
/// Note that typically the transaction ID are big-endian encoded.
#[inline]
pub fn transaction_hash(raw_transaction: &[u8]) -> [u8; 32] {
    let tx_id = digest(&SHA256, digest(&SHA256, &raw_transaction).as_ref());
    tx_id.as_ref().try_into().unwrap()
}

impl Transaction {
    /// Calculate the transaction hash in little-endian format. This is the double SHA256 digest of the raw transaction.
    ///
    /// Note that typically the transaction hash are big-endian encoded.
    #[inline]
    pub fn transaction_hash(&self) -> [u8; 32] {
        let mut raw_tx = Vec::with_capacity(self.encoded_len());
        self.encode_raw(&mut raw_tx);
        transaction_hash(&raw_tx)
    }

    /// Calculate the reversed transaction hash. Typically used in the
    /// lotusd-rpc hex encoding. This is the double SHA256 digest of the raw
    /// transaction in big-endian encoding.
    #[inline]
    pub fn transaction_hash_rev(&self) -> [u8; 32] {
        let mut raw_tx = Vec::with_capacity(self.encoded_len());
        self.encode_raw(&mut raw_tx);
        transaction_hash(&raw_tx)
    }

    /// Calculate the reversed transaction ID which is used in the lotusd rpc
    ///
    /// Note that typically the transaction ID are big-endian encoded.
    #[inline]
    pub fn transaction_id_rev(&self) -> [u8; 32] {
        let mut txid = self.transaction_id();
        txid.reverse();
        txid
    }

    /// Calculate the transaction ID. This is the double SHA256 digest of the raw transaction in big-endian encoding.
    #[inline]
    pub fn transaction_id(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(4 + 32 + 1 + 32 + 1 + 4);
        buf.put_u32_le(self.version);
        let mut inputleaves = Vec::with_capacity(self.inputs.len());
        for input in &self.inputs {
            let mut inputbuf = Vec::new();
            input.outpoint.encode_raw(&mut inputbuf);
            inputbuf.put_u32_le(input.sequence);
            inputleaves.push(merkle::sha256d(&inputbuf));
        }
        let (input_merkle, inputs_height) = merkle::lotus_merkle_root(inputleaves);
        buf.extend_from_slice(&input_merkle);
        buf.push(inputs_height); // height
        let mut outputleaves = Vec::with_capacity(self.inputs.len());
        for output in &self.outputs {
            let mut outputbuf = Vec::new();
            output.encode_raw(&mut outputbuf);
            outputleaves.push(merkle::sha256d(&outputbuf));
        }
        let (output_merkle, outputs_height) = merkle::lotus_merkle_root(outputleaves);
        buf.extend_from_slice(&output_merkle);
        buf.push(outputs_height); //height
        buf.put_u32_le(self.lock_time);
        merkle::sha256d(&buf)
    }

    /// Calculate input count VarInt.
    #[inline]
    fn input_count_varint(&self) -> VarInt {
        VarInt(self.inputs.len() as u64)
    }

    /// Calculate output count length.
    #[inline]
    fn output_count_varint(&self) -> VarInt {
        VarInt(self.outputs.len() as u64)
    }

    /// Calculate signature hash of a specific input.
    #[inline]
    pub fn signature_hash(
        &self,
        input_index: usize,
        script_pubkey: Script,
        sig_hash_type: SignatureHashType,
    ) -> Option<[u8; 32]> {
        // Special-case sighash_single bug because this is easy enough.
        if sig_hash_type == SignatureHashType::Single && input_index >= self.outputs.len() {
            const UNIT_HASH: [u8; 32] = [
                1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0,
            ];
            return Some(UNIT_HASH);
        }

        // Construct inputs
        let inputs = if sig_hash_type.is_anyone_can_pay() {
            let input = self.inputs.get(input_index)?.clone();
            vec![Input {
                outpoint: input.outpoint,
                script: script_pubkey,
                sequence: input.sequence,
            }]
        } else {
            self.inputs
                .iter()
                .enumerate()
                .map(|(local_index, input)| {
                    let sequence = if local_index != input_index
                        && (sig_hash_type == SignatureHashType::Single
                            || sig_hash_type == SignatureHashType::None)
                    {
                        0
                    } else {
                        input.sequence
                    };
                    let script = if local_index == input_index {
                        script_pubkey.clone()
                    } else {
                        Script::default()
                    };
                    Input {
                        outpoint: input.outpoint.clone(),
                        sequence,
                        script,
                    }
                })
                .collect()
        };

        // Construct outputs
        let outputs = match sig_hash_type {
            SignatureHashType::All => self.outputs.clone(),
            SignatureHashType::Single => self
                .outputs
                .iter()
                .take(input_index + 1)
                .enumerate()
                .map(|(local_index, output)| {
                    if local_index == input_index {
                        output.clone()
                    } else {
                        Output::default()
                    }
                })
                .collect(),
            SignatureHashType::None => vec![],
            _ => unreachable!(), // This is safe because we return earlier in these cases
        };

        // Construct transaction
        let transaction = Transaction {
            version: self.version,
            lock_time: self.lock_time,
            inputs,
            outputs,
        };

        // Serialize transaction
        let mut raw_transaction = Vec::with_capacity(transaction.encoded_len() + 4);
        transaction.encode_raw(&mut raw_transaction);
        let raw_sig_hash = (sig_hash_type as u32).to_le_bytes();
        raw_transaction.extend_from_slice(&raw_sig_hash);

        let pre_sig_hash: [u8; 32] = digest(&SHA256, digest(&SHA256, &raw_transaction).as_ref())
            .as_ref()
            .try_into()
            .unwrap();

        Some(pre_sig_hash)
    }
}

impl Encodable for Transaction {
    #[inline]
    fn encoded_len(&self) -> usize {
        let input_length_varint_length = self.input_count_varint().encoded_len();
        let input_total_length: usize = self.inputs.iter().map(|input| input.encoded_len()).sum();
        let output_length_varint_length = VarInt(self.outputs.len() as u64).encoded_len();
        let output_total_length: usize =
            self.outputs.iter().map(|output| output.encoded_len()).sum();
        4 + input_length_varint_length
            + input_total_length
            + output_length_varint_length
            + output_total_length
            + 4
    }

    #[inline]
    fn encode_raw<B: BufMut>(&self, buf: &mut B) {
        buf.put_u32_le(self.version);
        self.input_count_varint().encode_raw(buf);
        for input in &self.inputs {
            input.encode_raw(buf);
        }
        self.output_count_varint().encode_raw(buf);
        for output in &self.outputs {
            output.encode_raw(buf);
        }
        buf.put_u32_le(self.lock_time);
    }
}

/// Error associated with [`Transaction`] deserialization.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum DecodeError {
    /// Exhausted buffer when decoding `version` field.
    #[error("version too short")]
    VersionTooShort,
    /// Failed to decode input count [`VarInt`].
    #[error("input count: {0}")]
    InputCount(VarIntDecodeError),
    /// Failed to decode an input.
    #[error("input: {0}")]
    Input(InputDecodeError),
    /// Failed to decode output count [`VarInt`].
    #[error("output count: {0}")]
    OutputCount(VarIntDecodeError),
    /// Failed to decode an output.
    #[error("output: {0}")]
    Output(OutputDecodeError),
    /// Exhausted buffer when decoding `locktime` field.
    #[error("lock time too short")]
    LockTimeTooShort,
}

impl Decodable for Transaction {
    type Error = DecodeError;

    fn decode<B: Buf>(mut buf: &mut B) -> Result<Self, Self::Error> {
        // Parse version
        if buf.remaining() < 4 {
            return Err(Self::Error::VersionTooShort);
        }
        let version = buf.get_u32_le();

        // Parse inputs
        let n_inputs: u64 = VarInt::decode(&mut buf)
            .map_err(Self::Error::InputCount)?
            .into();
        let inputs: Vec<Input> = (0..n_inputs)
            .map(|_| Input::decode(buf))
            .collect::<Result<Vec<Input>, _>>()
            .map_err(Self::Error::Input)?;

        // Parse outputs
        let n_outputs: u64 = VarInt::decode(&mut buf)
            .map_err(Self::Error::OutputCount)?
            .into();
        let outputs: Vec<Output> = (0..n_outputs)
            .map(|_| Output::decode(buf))
            .collect::<Result<Vec<Output>, _>>()
            .map_err(Self::Error::Output)?;

        // Parse lock time
        if buf.remaining() < 4 {
            return Err(Self::Error::LockTimeTooShort);
        }
        let lock_time = buf.get_u32_le();
        Ok(Transaction {
            version,
            lock_time,
            inputs,
            outputs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode() {
        for hex_tx in test_txs() {
            let raw_tx = hex::decode(hex_tx).unwrap();
            Transaction::decode(&mut raw_tx.as_slice()).unwrap();
        }
    }

    #[test]
    fn encoded_len() {
        for hex_tx in test_txs() {
            let raw_tx_input = hex::decode(hex_tx).unwrap();
            let expected_len = raw_tx_input.len();
            let tx = Transaction::decode(&mut raw_tx_input.as_slice()).unwrap();

            let actual_len = tx.encoded_len();
            assert_eq!(expected_len, actual_len);
        }
    }

    #[test]
    fn encode() {
        for hex_tx in test_txs() {
            let raw_tx_input = hex::decode(hex_tx).unwrap();
            let tx = Transaction::decode(&mut raw_tx_input.as_slice()).unwrap();

            let buffer_len = tx.encoded_len();
            let mut raw_tx_output: Vec<u8> = Vec::with_capacity(buffer_len);
            tx.encode(&mut raw_tx_output).unwrap();
            assert_eq!(raw_tx_output, raw_tx_input);
        }
    }

    #[test]
    fn encode_insufficent_capacity() {
        for hex_tx in test_txs() {
            let raw_tx_input = hex::decode(hex_tx).unwrap();
            let tx = Transaction::decode(&mut raw_tx_input.as_slice()).unwrap();

            let mut raw_tx_output = Vec::with_capacity(0);
            assert!(tx.encode(&mut raw_tx_output.as_mut_slice()).is_err());
        }
    }

    #[test]
    fn test_txid_calculations() {
        for (hex_tx, hex_txid) in test_txs_for_txid() {
            let raw_tx_input = hex::decode(hex_tx).unwrap();
            let raw_tx_id = hex::decode(hex_txid).unwrap();

            let tx = Transaction::decode(&mut raw_tx_input.as_slice()).unwrap();

            assert_eq!(tx.transaction_id_rev().to_vec(), raw_tx_id);
        }
    }

    fn test_txs_for_txid() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "02000000010000000000000000000000000000000000000000000000000000000000000000ffffffff020000ffffffff0f00000000000000000b6a056c6f676f73034b9b006413a024000000001976a914745a32d27fe3ac28528591dd8a4ba6c4c4fe3d2988ac913cd1020000000017a914b6c79031b71d86ab0d617e1e1e706ec4ee34b07f87913cd102000000001976a914b8ae1c47effb58f72f7bca819fe7fc252f9e852e88ac913cd102000000001976a914b50b86a893d80c9e2ee72b199612374b7b4c1cd888ac913cd102000000001976a914da76a31b6760dcb90aa469c15965da6e80096e4588ac913cd102000000001976a9141325d2d8ba6e8c7d99ff66d21530917cc73429d288ac913cd102000000001976a9146a171891ab9443020bd2755ef79c6e59efc5926588ac913cd102000000001976a91419e8d8d0edab6ec43f04b656bff72af78d63ff6588ac913cd102000000001976a914c6492d4e44dcd0051e60a8add6af02b2f291b2aa88ac913cd102000000001976a914b5aeafec9f2972110c4c6af9508a3c41e1d3c73b88ac913cd102000000001976a9147c28aa91b93faf8aee0a6520a0a83f42dbc4a45b88ac913cd102000000001976a914b18eb08c4978e73480743b1598061d3cf38e10a888ac913cd102000000001976a9147d0893d1a278bab27e7ad92ed88bd7dceafd83a588ac913cd102000000001976a9144b869f9a55c57003df178bdc801184109d904f8b88ac00000000", 
                "92c8a467696f41fd9c171f4d53e1aff2932bb0a32b7ca81108fe0a9dc01d7aaf",
            ),
        ]
    }

    fn test_txs<'a>() -> Vec<&'a str> {
        vec![
            "907c2bc503ade11cc3b04eb2918b6f547b0630ab569273824748c87ea14b0696526c66ba740200000004ab65ababfd1f9bdd4ef073c7afc4ae00da8a66f429c917a0081ad1e1dabce28d373eab81d8628de802000000096aab5253ab52000052ad042b5f25efb33beec9f3364e8a9139e8439d9d7e26529c3c30b6c3fd89f8684cfd68ea0200000009ab53526500636a52ab599ac2fe02a526ed040000000008535300516352515164370e010000000003006300ab2ec229",
            "a0aa3126041621a6dea5b800141aa696daf28408959dfb2df96095db9fa425ad3f427f2f6103000000015360290e9c6063fa26912c2e7fb6a0ad80f1c5fea1771d42f12976092e7a85a4229fdb6e890000000001abc109f6e47688ac0e4682988785744602b8c87228fcef0695085edf19088af1a9db126e93000000000665516aac536affffffff8fe53e0806e12dfd05d67ac68f4768fdbe23fc48ace22a5aa8ba04c96d58e2750300000009ac51abac63ab5153650524aa680455ce7b000000000000499e50030000000008636a00ac526563ac5051ee030000000003abacabd2b6fe000000000003516563910fb6b5",
            "73107cbd025c22ebc8c3e0a47b2a760739216a528de8d4dab5d45cbeb3051cebae73b01ca10200000007ab6353656a636affffffffe26816dffc670841e6a6c8c61c586da401df1261a330a6c6b3dd9f9a0789bc9e000000000800ac6552ac6aac51ffffffff0174a8f0010000000004ac52515100000000",
            "50818f4c01b464538b1e7e7f5ae4ed96ad23c68c830e78da9a845bc19b5c3b0b20bb82e5e9030000000763526a63655352ffffffff023b3f9c040000000008630051516a6a5163a83caf01000000000553ab65510000000000",    
            "a93e93440250f97012d466a6cc24839f572def241c814fe6ae94442cf58ea33eb0fdd9bcc1030000000600636a0065acffffffff5dee3a6e7e5ad6310dea3e5b3ddda1a56bf8de7d3b75889fc024b5e233ec10f80300000007ac53635253ab53ffffffff0160468b04000000000800526a5300ac526a00000000",
            "d3b7421e011f4de0f1cea9ba7458bf3486bee722519efab711a963fa8c100970cf7488b7bb0200000003525352dcd61b300148be5d05000000000000000000",
            "04bac8c5033460235919a9c63c42b2db884c7c8f2ed8fcd69ff683a0a2cccd9796346a04050200000003655351fcad3a2c5a7cbadeb4ec7acc9836c3f5c3e776e5c566220f7f965cf194f8ef98efb5e3530200000007526a006552526526a2f55ba5f69699ece76692552b399ba908301907c5763d28a15b08581b23179cb01eac03000000075363ab6a516351073942c2025aa98a05000000000765006aabac65abd7ffa6030000000004516a655200000000",
            "c363a70c01ab174230bbe4afe0c3efa2d7f2feaf179431359adedccf30d1f69efe0c86ed390200000002ab51558648fe0231318b04000000000151662170000000000008ac5300006a63acac00000000",
            "8d437a7304d8772210a923fd81187c425fc28c17a5052571501db05c7e89b11448b36618cd02000000026a6340fec14ad2c9298fde1477f1e8325e5747b61b7e2ff2a549f3d132689560ab6c45dd43c3010000000963ac00ac000051516a447ed907a7efffebeb103988bf5f947fc688aab2c6a7914f48238cf92c337fad4a79348102000000085352ac526a5152517436edf2d80e3ef06725227c970a816b25d0b58d2cd3c187a7af2cea66d6b27ba69bf33a0300000007000063ab526553f3f0d6140386815d030000000003ab6300de138f00000000000900525153515265abac1f87040300000000036aac6500000000",
            "fd878840031e82fdbe1ad1d745d1185622b0060ac56638290ec4f66b1beef4450817114a2c0000000009516a63ab53650051abffffffff37b7a10322b5418bfd64fb09cd8a27ddf57731aeb1f1f920ffde7cb2dfb6cdb70300000008536a5365ac53515369ecc034f1594690dbe189094dc816d6d57ea75917de764cbf8eccce4632cbabe7e116cd0100000003515352ffffffff035777fc000000000003515200abe9140300000000050063005165bed6d10200000000076300536363ab65195e9110",
            "a63bc673049c75211aa2c09ecc38e360eaa571435fedd2af1116b5c1fa3d0629c269ecccbf0000000008ac65ab516352ac52ffffffffbf1a76fdda7f451a5f0baff0f9ccd0fe9136444c094bb8c544b1af0fa2774b06010000000463535253ffffffff13d6b7c3ddceef255d680d87181e100864eeb11a5bb6a3528cb0d70d7ee2bbbc02000000056a0052abab951241809623313b198bb520645c15ec96bfcc74a2b0f3db7ad61d455cc32db04afc5cc702000000016309c9ae25014d9473020000000004abab6aac3bb1e803",
            "4c565efe04e7d32bac03ae358d63140c1cfe95de15e30c5b84f31bb0b65bb542d637f49e0f010000000551abab536348ae32b31c7d3132030a510a1b1aacf7b7c3f19ce8dc49944ef93e5fa5fe2d356b4a73a00100000009abac635163ac00ab514c8bc57b6b844e04555c0a4f4fb426df139475cd2396ae418bc7015820e852f711519bc202000000086a00510000abac52488ff4aec72cbcfcc98759c58e20a8d2d9725aa4a80f83964e69bc4e793a4ff25cd75dc701000000086a52ac6aac5351532ec6b10802463e0200000000000553005265523e08680100000000002f39a6b0",
            "fd22692802db8ae6ab095aeae3867305a954278f7c076c542f0344b2591789e7e33e4d29f4020000000151ffffffffb9409129cfed9d3226f3b6bab7a2c83f99f48d039100eeb5796f00903b0e5e5e0100000006656552ac63abd226abac0403e649000000000007abab51ac5100ac8035f10000000000095165006a63526a52510d42db030000000007635365ac6a63ab24ef5901000000000453ab6a0000000000",
            "a43f85f701ffa54a3cc57177510f3ea28ecb6db0d4431fc79171cad708a6054f6e5b4f89170000000008ac6a006a536551652bebeaa2013e779c05000000000665ac5363635100000000",
            "c2b0b99001acfecf7da736de0ffaef8134a9676811602a6299ba5a2563a23bb09e8cbedf9300000000026300ffffffff042997c50300000000045252536a272437030000000007655353ab6363ac663752030000000002ab6a6d5c900000000000066a6a5265abab00000000",
            "82f9f10304c17a9d954cf3380db817814a8c738d2c811f0412284b2c791ec75515f38c4f8c020000000265ab5729ca7db1b79abee66c8a757221f29280d0681355cb522149525f36da760548dbd7080a0100000001510b477bd9ce9ad5bb81c0306273a3a7d051e053f04ecf3a1dbeda543e20601a5755c0cfae030000000451ac656affffffff71141a04134f6c292c2e0d415e6705dfd8dcee892b0d0807828d5aeb7d11f5ef0300000001520b6c6dc802a6f3dd0000000000056aab515163bfb6800300000000015300000000",
            "2074bad5011847f14df5ea7b4afd80cd56b02b99634893c6e3d5aaad41ca7c8ee8e5098df003000000026a6affffffff018ad59700000000000900ac656a526551635300000000",
            "7100b11302e554d4ef249ee416e7510a485e43b2ba4b8812d8fe5529fe33ea75f36d392c4403000000020000ffffffff3d01a37e075e9a7715a657ae1bdf1e44b46e236ad16fd2f4c74eb9bf370368810000000007636553ac536365ffffffff01db696a0400000000065200ac656aac00000000",
            "02c1017802091d1cb08fec512db7b012fe4220d57a5f15f9e7676358b012786e1209bcff950100000004acab6352ffffffff799bc282724a970a6fea1828984d0aeb0f16b67776fa213cbdc4838a2f1961a3010000000951516a536552ab6aabffffffff016c7b4b03000000000865abac5253ac5352b70195ad", 
            "4504cb1904c7a4acf375ddae431a74de72d5436efc73312cf8e9921f431267ea6852f9714a01000000066a656a656553a2fbd587c098b3a1c5bd1d6480f730a0d6d9b537966e20efc0e352d971576d0f87df0d6d01000000016321aeec3c4dcc819f1290edb463a737118f39ab5765800547522708c425306ebfca3f396603000000055300ac656a1d09281d05bfac57b5eb17eb3fa81ffcedfbcd3a917f1be0985c944d473d2c34d245eb350300000007656a51525152ac263078d9032f470f0500000000066aac00000052e12da60200000000003488410200000000076365006300ab539981e432",
            "14bc7c3e03322ec0f1311f4327e93059c996275302554473104f3f7b46ca179bfac9ef753503000000016affffffff9d405eaeffa1ca54d9a05441a296e5cc3a3e32bb8307afaf167f7b57190b07e00300000008abab51ab5263abab45533aa242c61bca90dd15d46079a0ab0841d85df67b29ba87f2393cd764a6997c372b55030000000452005263ffffffff0250f40e02000000000651516a0063630e95ab0000000000046a5151ac00000000",
            "a08ff466049fb7619e25502ec22fedfb229eaa1fe275aa0b5a23154b318441bf547989d0510000000005ab5363636affffffff2b0e335cb5383886751cdbd993dc0720817745a6b1c9b8ab3d15547fc9aafd03000000000965656a536a52656a532b53d10584c290d3ac1ab74ab0a19201a4a039cb59dc58719821c024f6bf2eb26322b33f010000000965ac6aac0053ab6353ffffffff048decba6ebbd2db81e416e39dde1f821ba69329725e702bcdea20c5cc0ecc6402000000086363ab5351ac6551466e377b0468c0fa00000000000651ab53ac6a513461c6010000000008636a636365535100eeb3dc010000000006526a52ac516a43f362010000000005000063536500000000",
            "f10a0356031cd569d652dbca8e7a4d36c8da33cdff428d003338602b7764fe2c96c505175b010000000465ac516affffffffbb54563c71136fa944ee20452d78dc87073ac2365ba07e638dce29a5d179da600000000003635152ffffffff9a411d8e2d421b1e6085540ee2809901e590940bbb41532fa38bd7a16b68cc350100000007535251635365636195df1603b61c45010000000002ab65bf6a310400000000026352fcbba10200000000016aa30b7ff0",
            "c3325c9b012f659466626ca8f3c61dfd36f34670abc054476b7516a1839ec43cd0870aa0c0000000000753525265005351e7e3f04b0112650500000000000363ac6300000000",
            "b500ca48011ec57c2e5252e5da6432089130603245ffbafb0e4c5ffe6090feb629207eeb0e010000000652ab6a636aab8302c9d2042b44f40500000000015278c05a050000000004ac5251524be080020000000007636aac63ac5252c93a9a04000000000965ab6553636aab5352d91f9ddb",
            "f52ff64b02ee91adb01f3936cc42e41e1672778962b68cf013293d649536b519bc3271dd2c00000000020065afee11313784849a7c15f44a61cd5fd51ccfcdae707e5896d131b082dc9322a19e12858501000000036aac654e8ca882022deb7c020000000006006a515352abd3defc0000000000016300000000", 
            "ab7d6f36027a7adc36a5cf7528fe4fb5d94b2c96803a4b38a83a675d7806dda62b380df86a0000000003000000ffffffff5bc00131e29e22057c04be854794b4877dda42e416a7a24706b802ff9da521b20000000007ac6a0065ac52ac957cf45501b9f06501000000000500ac6363ab25f1110b", 
            "f940888f023dce6360263c850372eb145b864228fdbbb4c1186174fa83aab890ff38f8c9a90300000000ffffffff01e80ccdb081e7bbae1c776531adcbfb77f2e5a7d0e5d0d0e2e6c8758470e85f00000000020053ffffffff03b49088050000000004656a52ab428bd604000000000951630065ab63ac636a0cbacf0400000000070063ac5265ac53d6e16604", 
            "530ecd0b01ec302d97ef6f1b5a6420b9a239714013e20d39aa3789d191ef623fc215aa8b940200000005ac5351ab6a3823ab8202572eaa04000000000752ab6a51526563fd8a270100000000036a006581a798f0", 
            "5d781d9303acfcce964f50865ddfddab527ea971aee91234c88e184979985c00b4de15204b0100000003ab6352a009c8ab01f93c8ef2447386c434b4498538f061845862c3f9d5751ad0fce52af442b3a902000000045165ababb909c66b5a3e7c81b3c45396b944be13b8aacfc0204f3f3c105a66fa8fa6402f1b5efddb01000000096a65ac636aacab656ac3c677c402b79fa4050000000004006aab5133e35802000000000751ab635163ab0078c2e025",
            "a9c57b1a018551bcbc781b256642532bbc09967f1cbe30a227d352a19365d219d3f11649a3030000000451655352b140942203182894030000000006ab00ac6aab654add350400000000003d379505000000000553abacac00e1739d36",
            "05c4fb94040f5119dc0b10aa9df054871ed23c98c890f1e931a98ffb0683dac45e98619fdc0200000007acab6a525263513e7495651c9794c4d60da835d303eb4ee6e871f8292f6ad0b32e85ef08c9dc7aa4e03c9c010000000500ab52acacfffffffffee953259cf14ced323fe8d567e4c57ba331021a1ef5ac2fa90f7789340d7c550100000007ac6aacac6a6a53ffffffff08d9dc820d00f18998af247319f9de5c0bbd52a475ea587f16101af3afab7c210100000003535363569bca7c0468e34f00000000000863536353ac51ac6584e319010000000006650052ab6a533debea030000000003ac0053ee7070020000000006ac52005253ac00000000",
            "ca66ae10049533c2b39f1449791bd6d3f039efe0a121ab7339d39ef05d6dcb200ec3fb2b3b020000000465006a53ffffffff534b8f97f15cc7fb4f4cea9bf798472dc93135cd5b809e4ca7fe4617a61895980100000000ddd83c1dc96f640929dd5e6f1151dab1aa669128591f153310d3993e562cc7725b6ae3d903000000046a52536582f8ccddb8086d8550f09128029e1782c3f2624419abdeaf74ecb24889cc45ac1a64492a0100000002516a4867b41502ee6ccf03000000000752acacab52ab6a4b7ba80000000000075151ab0052536300000000", 
            "ba646d0b0453999f0c70cb0430d4cab0e2120457bb9128ed002b6e9500e9c7f8d7baa20abe0200000001652a4e42935b21db02b56bf6f08ef4be5adb13c38bc6a0c3187ed7f6197607ba6a2c47bc8a03000000040052516affffffffa55c3cbfc19b1667594ac8681ba5d159514b623d08ed4697f56ce8fcd9ca5b0b00000000096a6a5263ac655263ab66728c2720fdeabdfdf8d9fb2bfe88b295d3b87590e26a1e456bad5991964165f888c03a0200000006630051ac00acffffffff0176fafe0100000000070063acac65515200000000", 
            "2ddb8f84039f983b45f64a7a79b74ff939e3b598b38f436def7edd57282d0803c7ef34968d02000000026a537eb00c4187de96e6e397c05f11915270bcc383959877868ba93bac417d9f6ed9f627a7930300000004516551abffffffffacc12f1bb67be3ae9f1d43e55fda8b885340a0df1175392a8bbd9f959ad3605003000000025163ffffffff02ff0f4700000000000070bd99040000000003ac53abf8440b42", 
            "b21fc15403b4bdaa994204444b59323a7b8714dd471bd7f975a4e4b7b48787e720cbd1f5f00000000000ffffffff311533001cb85c98c1d58de0a5fbf27684a69af850d52e22197b0dc941bc6ca9030000000765ab6363ab5351a8ae2c2c7141ece9a4ff75c43b7ea9d94ec79b7e28f63e015ac584d984a526a73fe1e04e0100000007526352536a5365ffffffff02a0a9ea030000000002ab52cfc4f300000000000465525253e8e0f342", 
            "f821a042036ad43634d29913b77c0fc87b4af593ac86e9a816a9d83fd18dfcfc84e1e1d57102000000076a63ac52006351ffffffffbcdaf490fc75086109e2f832c8985716b3a624a422cf9412fe6227c10585d21203000000095252abab5352ac526affffffff2efed01a4b73ad46c7f7bc7fa3bc480f8e32d741252f389eaca889a2e9d2007e000000000353ac53ffffffff032ac8b3020000000009636300000063516300d3d9f2040000000006510065ac656aafa5de0000000000066352ab5300ac9042b57d", 
            "d62f183e037e0d52dcf73f9b31f70554bce4f693d36d17552d0e217041e01f15ad3840c838000000000963acac6a6a6a63ab63ffffffffabdfb395b6b4e63e02a763830f536fc09a35ff8a0cf604021c3c751fe4c88f4d0300000006ab63ab65ac53aa4d30de95a2327bccf9039fb1ad976f84e0b4a0936d82e67eafebc108993f1e57d8ae39000000000165ffffffff04364ad30500000000036a005179fd84010000000007ab636aac6363519b9023030000000008510065006563ac6acd2a4a02000000000000000000", 
            "44c200a5021238de8de7d80e7cce905606001524e21c8d8627e279335554ca886454d692e6000000000500acac52abbb8d1dc876abb1f514e96b21c6e83f429c66accd961860dc3aed5071e153e556e6cf076d02000000056553526a51870a928d0360a580040000000004516a535290e1e302000000000851ab6a00510065acdd7fc5040000000007515363ab65636abb1ec182", 
            "d682d52d034e9b062544e5f8c60f860c18f029df8b47716cabb6c1b4a4b310a0705e754556020000000400656a0016eeb88eef6924fed207fba7ddd321ff3d84f09902ff958c815a2bf2bb692eb52032c4d803000000076365ac516a520099788831f8c8eb2552389839cfb81a9dc55ecd25367acad4e03cfbb06530f8cccf82802701000000085253655300656a53ffffffff02d543200500000000056a510052ac03978b05000000000700ac51525363acfdc4f784", 
            "e8c0dec5026575ddf31343c20aeeca8770afb33d4e562aa8ee52eeda6b88806fdfd4fe0a97030000000953acabab65ab516552ffffffffdde122c2c3e9708874286465f8105f43019e837746686f442666629088a970e0010000000153ffffffff01f98eee0100000000025251fe87379a",
            "b288c331011c17569293c1e6448e33a64205fc9dc6e35bc756a1ac8b97d18e912ea88dc0770200000007635300ac6aacabfc3c890903a3ccf8040000000004656500ac9c65c9040000000009ab6a6aabab65abac63ac5f7702000000000365005200000000",
            "6b68ba00023bb4f446365ea04d68d48539aae66f5b04e31e6b38b594d2723ab82d44512460000000000200acffffffff5dfc6febb484fff69c9eeb7c7eb972e91b6d949295571b8235b1da8955f3137b020000000851ac6352516a535325828c8a03365da801000000000800636aabac6551ab0f594d03000000000963ac536365ac63636a45329e010000000005abac53526a00000000", 
            "046ac25e030a344116489cc48025659a363da60bc36b3a8784df137a93b9afeab91a04c1ed020000000951ab0000526a65ac51ffffffff6c094a03869fde55b9a8c4942a9906683f0a96e2d3e5a03c73614ea3223b2c29020000000500ab636a6affffffff3da7aa5ecef9071600866267674b54af1740c5aeb88a290c459caa257a2683cb0000000004ab6565ab7e2a1b900301b916030000000005abac63656308f4ed03000000000852ab53ac63ac51ac73d620020000000003ab00008deb1285", 
            "ff5400dd02fec5beb9a396e1cbedc82bedae09ed44bae60ba9bef2ff375a6858212478844b03000000025253ffffffff01e46c203577a79d1172db715e9cc6316b9cfc59b5e5e4d9199fef201c6f9f0f000000000900ab6552656a5165acffffffff02e8ce62040000000002515312ce3e00000000000251513f119316",
            "b54bf5ac043b62e97817abb892892269231b9b220ba08bc8dbc570937cd1ea7cdc13d9676c010000000451ab5365a10adb7b35189e1e8c00b86250f769319668189b7993d6bdac012800f1749150415b2deb0200000003655300ffffffff60b9f4fb9a7e17069fd00416d421f804e2ef2f2c67de4ca04e0241b9f9c1cc5d0200000003ab6aacfffffffff048168461cce1d40601b42fbc5c4f904ace0d35654b7cc1937ccf53fe78505a0100000008526563525265abacffffffff01dbf4e6040000000007acac656553636500000000",
            "ebf628b30360bab3fa4f47ce9e0dcbe9ceaf6675350e638baff0c2c197b2419f8e4fb17e16000000000452516365ac4d909a79be207c6e5fb44fbe348acc42fc7fe7ef1d0baa0e4771a3c4a6efdd7e2c118b0100000003acacacffffffffa6166e9101f03975721a3067f1636cc390d72617be72e5c3c4f73057004ee0ee010000000863636a6a516a5252c1b1e82102d8d54500000000000153324c900400000000015308384913",
            "d6a8500303f1507b1221a91adb6462fb62d741b3052e5e7684ea7cd061a5fc0b0e93549fa50100000004acab65acfffffffffdec79bf7e139c428c7cfd4b35435ae94336367c7b5e1f8e9826fcb0ebaaaea30300000000ffffffffd115fdc00713d52c35ea92805414bd57d1e59d0e6d3b79a77ee18a3228278ada020000000453005151ffffffff040231510300000000085100ac6a6a000063c6041c0400000000080000536a6563acac138a0b04000000000263abd25fbe03000000000900656a00656aac510000000000",
            "e92492cc01aec4e62df67ea3bc645e2e3f603645b3c5b353e4ae967b562d23d6e043badecd0100000003acab65ffffffff02c7e5ea040000000002ab52e1e584010000000005536365515195d16047",
            "b28d5f5e015a7f24d5f9e7b04a83cd07277d452e898f78b50aae45393dfb87f94a26ef57720200000008ababac630053ac52ffffffff046475ed040000000008ab5100526363ac65c9834a04000000000251abae26b30100000000040000ac65ceefb900000000000000000000", 
            "efb8b09801f647553b91922a5874f8e4bb2ed8ddb3536ed2d2ed0698fac5e0e3a298012391030000000952ac005263ac52006affffffff04cdfa0f050000000007ac53ab51abac65b68d1b02000000000553ab65ac00d057d50000000000016a9e1fda010000000007ac63ac536552ac00000000",
            "67761f2a014a16f3940dcb14a22ba5dc057fcffdcd2cf6150b01d516be00ef55ef7eb07a830100000004636a6a51ffffffff01af67bd050000000008526553526300510000000000", 
            "ec02fbee03120d02fde12574649660c441b40d330439183430c6feb404064d4f507e704f3c0100000000ffffffffe108d99c7a4e5f75cc35c05debb615d52fac6e3240a6964a29c1704d98017fb60200000002ab63fffffffff726ec890038977adfc9dadbeaf5e486d5fcb65dc23acff0dd90b61b8e2773410000000002ac65e9dace55010f881b010000000005ac00ab650000000000",
            "33b03bf00222c7ca35c2f8870bbdef2a543b70677e413ce50494ac9b22ea673287b6aa55c50000000005ab00006a52ee4d97b527eb0b427e4514ea4a76c81e68c34900a23838d3e57d0edb5410e62eeb8c92b6000000000553ac6aacac42e59e170326245c000000000009656553536aab516aabb1a10603000000000852ab52ab6a516500cc89c802000000000763ac6a63ac516300000000", 
            "813eda1103ac8159850b4524ef65e4644e0fc30efe57a5db0c0365a30446d518d9b9aa8fdd0000000003656565c2f1e89448b374b8f12055557927d5b33339c52228f7108228149920e0b77ef0bcd69da60000000006abac00ab63ab82cdb7978d28630c5e1dc630f332c4245581f787936f0b1e84d38d33892141974c75b4750300000004ac53ab65ffffffff0137edfb02000000000000000000", 
            "9e45d9aa0248c16dbd7f435e8c54ae1ad086de50c7b25795a704f3d8e45e1886386c653fbf01000000025352fb4a1acefdd27747b60d1fb79b96d14fb88770c75e0da941b7803a513e6d4c908c6445c7010000000163ffffffff014069a8010000000001520a794fb3",
            "c3efabba03cb656f154d1e159aa4a1a4bf9423a50454ebcef07bc3c42a35fb8ad84014864d0000000000d1cc73d260980775650caa272e9103dc6408bdacaddada6b9c67c88ceba6abaa9caa2f7d020000000553536a5265ffffffff9f946e8176d9b11ff854b76efcca0a4c236d29b69fb645ba29d406480427438e01000000066a0065005300ffffffff040419c0010000000003ab6a63cdb5b6010000000009006300ab5352656a63f9fe5e050000000004acac5352611b980100000000086a00acac00006a512d7f0c40",
            "efb55c2e04b21a0c25e0e29f6586be9ef09f2008389e5257ebf2f5251051cdc6a79fce2dac020000000351006affffffffaba73e5b6e6c62048ba5676d18c33ccbcb59866470bb7911ccafb2238cfd493802000000026563ffffffffe62d7cb8658a6eca8a8babeb0f1f4fa535b62f5fc0ec70eb0111174e72bbec5e0300000009abababac516365526affffffffbf568789e681032d3e3be761642f25e46c20322fa80346c1146cb47ac999cf1b0300000000b3dbd55902528828010000000001ab0aac7b0100000000015300000000", 
            "91d3b21903629209b877b3e1aef09cd59aca6a5a0db9b83e6b3472aceec3bc2109e64ab85a0200000003530065ffffffffca5f92de2f1b7d8478b8261eaf32e5656b9eabbc58dcb2345912e9079a33c4cd010000000700ab65ab00536ad530611da41bbd51a389788c46678a265fe85737b8d317a83a8ff7a839debd18892ae5c80300000007ab6aac65ab51008b86c501038b8a9a05000000000263525b3f7a040000000007ab535353ab00abd4e3ff04000000000665ac51ab65630b7b656f", 
            "ceecfa6c02b7e3345445b82226b15b7a097563fa7d15f3b0c979232b138124b62c0be007890200000009abac51536a63525253ffffffffbae481ccb4f15d94db5ec0d8854c24c1cc8642bd0c6300ede98a91ca13a4539a0200000001ac50b0813d023110f5020000000006acabac526563e2b0d0040000000009656aac0063516a536300000000", 
            "f06f64af04fdcb830464b5efdb3d5ee25869b0744005375481d7b9d7136a0eb8828ad1f0240200000003516563fffffffffd3ba192dabe9c4eb634a1e3079fca4f072ee5ceb4b57deb6ade5527053a92c5000000000165ffffffff39f43401a36ba13a5c6dd7f1190e793933ae32ee3bf3e7bfb967be51e681af760300000009650000536552636a528e34f50b21183952cad945a83d4d56294b55258183e1627d6e8fb3beb8457ec36cadb0630000000005abab530052334a7128014bbfd10100000000085352ab006a63656afc424a7c",
            "6dfd2f98046b08e7e2ef5fff153e00545faf7076699012993c7a30cb1a50ec528281a9022f030000000152ffffffff1f535e4851920b968e6c437d84d6ecf586984ebddb7d5db6ae035bd02ba222a8010000000651006a53ab51605072acb3e17939fa0737bc3ee43bc393b4acd58451fc4ffeeedc06df9fc649828822d5010000000253525a4955221715f27788d302382112cf60719be9ae159c51f394519bd5f7e70a4f9816c7020200000009526a6a51636aab656a36d3a5ff0445548e0100000000086a6a00516a52655167030b050000000004ac6a63525cfda8030000000000e158200000000000010000000000", 
            "2a45cd1001bf642a2315d4a427eddcc1e2b0209b1c6abd2db81a800c5f1af32812de42032702000000050051525200ffffffff032177db050000000005530051abac49186f000000000004ab6aab00645c0000000000000765655263acabac00000000", 
            "ca816e7802cd43d66b9374cd9bf99a8da09402d69c688d8dcc5283ace8f147e1672b757e020200000005516aabab5240fb06c95c922342279fcd88ba6cd915933e320d7becac03192e0941e0345b79223e89570300000004005151ac353ecb5d0264dfbd010000000005ac6aacababd5d70001000000000752ac53ac6a5151ec257f71",
            "b42b955303942fedd7dc77bbd9040aa0de858afa100f399d63c7f167b7986d6c2377f66a7403000000066aac00525100ffffffff0577d04b64880425a3174055f94191031ad6b4ca6f34f6da9be7c3411d8b51fc000000000300526a6391e1cf0f22e45ef1c44298523b516b3e1249df153590f592fcb5c5fc432dc66f3b57cb03000000046a6aac65ffffffff0393a6c9000000000004516a65aca674ac0400000000046a525352c82c370000000000030053538e577f89", 
            "92c9fe210201e781b72554a0ed5e22507fb02434ddbaa69aff6e74ea8bad656071f1923f3f02000000056a63ac6a514470cef985ba83dcb8eee2044807bedbf0d983ae21286421506ae276142359c8c6a34d68020000000863ac63525265006aa796dd0102ca3f9d05000000000800abab52ab535353cd5c83010000000007ac00525252005322ac75ee",
            "ccca1d5b01e40fe2c6b3ee24c660252134601dab785b8f55bd6201ffaf2fddc7b3e2192325030000000365535100496d4703b4b66603000000000665535253ac633013240000000000015212d2a502000000000951abac636353636a5337b82426",
            "bc1a7a3c01691e2d0c4266136f12e391422f93655c71831d90935fbda7e840e50770c61da20000000008635253abac516353ffffffff031f32aa020000000003636563786dbc0200000000003e950f00000000000563516a655184b8a1de",
            "076d209e02d904a6c40713c7225d23e7c25d4133c3c3477828f98c7d6dbd68744023dbb66b030000000753ab00536565acffffffff10975f1b8db8861ca94c8cc7c7cff086ddcd83e10b5fffd4fc8f2bdb03f9463c0100000000ffffffff029dff76010000000006526365530051a3be6004000000000000000000",
            "cac6382d0462375e83b67c7a86c922b569a7473bfced67f17afd96c3cd2d896cf113febf9e0300000003006a53ffffffffaa4913b7eae6821487dd3ca43a514e94dcbbf350f8cc4cafff9c1a88720711b800000000096a6a525300acac6353ffffffff184fc4109c34ea27014cc2c1536ef7ed1821951797a7141ddacdd6e429fae6ff01000000055251655200ffffffff9e7b79b4e6836e290d7b489ead931cba65d1030ccc06f20bd4ca46a40195b33c030000000008f6bc8304a09a2704000000000563655353511dbc73050000000000cf34c500000000000091f76e0000000000085200ab00005100abd07208cb", 
            "1711146502c1a0b82eaa7893976fefe0fb758c3f0e560447cef6e1bde11e42de91a125f71c030000000015bd8c04703b4030496c7461482481f290c623be3e76ad23d57a955807c9e851aaaa20270300000000d04abaf20326dcb7030000000001632225350400000000075263ac00520063dddad9020000000000af23d148", 
            "22d81c740469695a6a83a9a4824f77ecff8804d020df23713990afce2b72591ed7de98500502000000065352526a6a6affffffff90dc85e118379b1005d7bbc7d2b8b0bab104dad7eaa49ff5bead892f17d8c3ba010000000665656300ab51ffffffff965193879e1d5628b52005d8560a35a2ba57a7f19201a4045b7cbab85133311d0200000003ac005348af21a13f9b4e0ad90ed20bf84e4740c8a9d7129632590349afc03799414b76fd6e826200000000025353ffffffff04a0d40d04000000000060702700000000000652655151516ad31f1502000000000365ac0069a1ac0500000000095100655300ab53525100000000", 
            "a27dcbc801e3475174a183586082e0914c314bc9d79d1570f29b54591e5e0dff07fbb45a7f0000000004ac53ab51ffffffff027347f5020000000005535351ab63d0e5c9030000000009ac65ab6a63515200ab7cd632ed",
            "0728c606014c1fd6005ccf878196ba71a54e86cc8c53d6db500c3cc0ac369a26fac6fcbc210000000005ab53ac5365ba9668290182d7870100000000066a000053655100000000", 
            "ed3bb93802ddbd08cb030ef60a2247f715a0226de390c9c1a81d52e83f8674879065b5f87d0300000003ab6552ffffffff04d2c5e60a21fb6da8de20bf206db43b720e2a24ce26779bca25584c3f765d1e0200000008ab656a6aacab00ab6e946ded025a811d04000000000951abac6352ac00ab5143cfa3030000000005635200636a00000000",
            "59f4629d030fa5d115c33e8d55a79ea3cba8c209821f979ed0e285299a9c72a73c5bba00150200000002636affffffffd8aca2176df3f7a96d0dc4ee3d24e6cecde1582323eec2ebef9a11f8162f17ac0000000007ab6565acab6553ffffffffeebc10af4f99c7a21cbc1d1074bd9f0ee032482a71800f44f26ee67491208e0403000000065352ac656351ffffffff0434e955040000000004ab515152caf2b305000000000365ac007b1473030000000003ab530033da970500000000060051536a5253bb08ab51", 
            "33a98004029d262f951881b20a8d746c8c707ea802cd2c8b02a33b7e907c58699f97e42be80100000007ac53536552abacdee04cc01d205fd8a3687fdf265b064d42ab38046d76c736aad8865ca210824b7c622ecf02000000070065006a536a6affffffff01431c5d010000000000270d48ee",
            "aac18f2b02b144ed481557c53f2146ae523f24fcde40f3445ab0193b6b276c315dc2894d2300000000075165650000636a233526947dbffc76aec7db1e1baa6868ad4799c76e14794dcbaaec9e713a83967f6a65170200000005abac6551ab27d518be01b652a30000000000015300000000",
            "5ab79881033555b65fe58c928883f70ce7057426fbdd5c67d7260da0fe8b1b9e6a2674cb850300000009ac516aac6aac006a6affffffffa5be9223b43c2b1a4d120b5c5b6ec0484f637952a3252181d0f8e813e76e11580200000000e4b5ceb8118cb77215bbeedc9a076a4d087bb9cd1473ea32368b71daeeeacc451ec209010000000005acac5153aced7dc34e02bc5d11030000000005ac5363006a54185803000000000552ab00636a00000000", 
            "6c2c8fac0124b0b7d4b610c3c5b91dee32b7c927ac71abdf2d008990ca1ac40de0dfd530660300000006ababac5253656bd7eada01d847ec000000000004ac52006af4232ec8", 
            "0e803682024f79337b25c98f276d412bc27e56a300aa422c42994004790cee213008ff1b8303000000080051ac65ac655165f421a331892b19a44c9f88413d057fea03c3c4a6c7de4911fe6fe79cf2e9b3b10184b1910200000005525163630096cb1c670398277204000000000253acf7d5d502000000000963536a6a636a5363ab381092020000000002ac6a911ccf32",
            "7d71669d03022f9dd90edac323cde9e56354c6804c6b8e687e9ae699f46805aafb8bcaa636000000000253abffffffff698a5fdd3d7f2b8b000c68333e4dd58fa8045b3e2f689b889beeb3156cecdb490300000009525353abab0051acabc53f0aa821cdd69b473ec6e6cf45cf9b38996e1c8f52c27878a01ec8bb02e8cb31ad24e500000000055353ab0052ffffffff0447a23401000000000565ab53ab5133aaa0030000000006515163656563057d110300000000056a6aacac52cf13b5000000000003526a5100000000", 
            "9ff618e60136f8e6bb7eabaaac7d6e2535f5fba95854be6d2726f986eaa9537cb283c701ff02000000026a65ffffffff012d1c0905000000000865ab00ac6a516a652f9ad240", 
            "09adb2e90175ca0e816326ae2dce7750c1b27941b16f6278023dbc294632ab97977852a09d030000000465ab006affffffff027739cf0100000000075151ab63ac65ab8a5bb601000000000653ac5151520011313cdc",
            "fd22ebaa03bd588ad16795bea7d4aa7f7d48df163d75ea3afebe7017ce2f350f6a0c1cb0bb00000000086aabac5153526363ffffffff488e0bb22e26a565d77ba07178d17d8f85702630ee665ec35d152fa05af3bda10200000004515163abffffffffeb21035849e85ad84b2805e1069a91bb36c425dc9c212d9bae50a95b6bfde1200300000001ab5df262fd02b69848040000000008ab6363636a6363ace23bf2010000000007655263635253534348c1da", 
            "b04154610363fdade55ceb6942d5e5a723323863b48a0cb04fdcf56210717955763f56b08d0300000009ac526a525151635151ffffffff93a176e76151a9eabdd7af00ef2af72f9e7af5ecb0aa4d45d00618f394cdd03c030000000074d818b332ebe05dc24c44d776cf9d275c61f471cc01efce12fd5a16464157f1842c65cb00000000066a0000ac6352d3c4134f01d8a1c0030000000005520000005200000000",
            "94533db7015e70e8df715066efa69dbb9c3a42ff733367c18c22ff070392f988f3b93920820000000006535363636300ce4dac3e03169af80300000000080065ac6a53ac65ac39c050020000000006abacab6aacac708a02050000000005ac5251520000000000", 
            "c8597ada04f59836f06c224a2640b79f3a8a7b41ef3efa2602592ddda38e7597da6c639fee0300000009005251635351acabacffffffff4c518f347ee694884b9d4072c9e916b1a1f0a7fc74a1c90c63fdf8e5a185b6ae02000000007113af55afb41af7518ea6146786c7c726641c68c8829a52925e8d4afd07d8945f68e7230300000008ab00ab65ab650063ffffffffc28e46d7598312c420e11dfaae12add68b4d85adb182ae5b28f8340185394b63000000000165ffffffff04dbabb7010000000000ee2f6000000000000852ab6500ab6a51acb62a27000000000009ac53515300ac006a6345fb7505000000000752516a0051636a00000000",
            "c33028b301d5093e1e8397270d75a0b009b2a6509a01861061ab022ca122a6ba935b8513320200000000ffffffff013bcf5a0500000000015200000000",
            "43b2727901a7dd06dd2abf690a1ccedc0b0739cb551200796669d9a25f24f71d8d101379f50300000000ffffffff0418e031040000000000863d770000000000085352ac526563ac5174929e040000000004ac65ac00ec31ac0100000000066a51ababab5300000000", 
            "fe6ddf3a02657e42a7496ef170b4a8caf245b925b91c7840fd28e4a22c03cb459cb498b8d603000000065263656a650071ce6bf8d905106f9f1faf6488164f3decac65bf3c5afe1dcee20e6bc3cb6d052561985a030000000163295b117601343dbb0000000000026563dba521df",
            "c61523ef0129bb3952533cbf22ed797fa2088f307837dd0be1849f20decf709cf98c6f032f03000000026563c0f1d378044338310400000000066363516a5165a14fcb0400000000095163536a6a00ab53657271d60200000000001d953f0500000000010000000000", 
            "ba3dac6c0182562b0a26d475fe1e36315f0913b6869bdad0ecf21f1339a5fcbccd32056c840200000000ffffffff04300351050000000000220ed405000000000851abac636565ac53dbbd19020000000007636363ac6a52acbb005a0500000000016abd0c78a8", 
            "ac27e7f5025fc877d1d99f7fc18dd4cadbafa50e34e1676748cc89c202f93abf36ed46362101000000036300abffffffff958cd5381962b765e14d87fc9524d751e4752dd66471f973ed38b9d562e525620100000003006500ffffffff02b67120050000000004ac51516adc330c0300000000015200000000", 
            "7e50207303146d1f7ad62843ae8017737a698498d4b9118c7a89bb02e8370307fa4fada41d000000000753006300005152b7afefc85674b1104ba33ef2bf37c6ed26316badbc0b4aa6cb8b00722da4f82ff3555a6c020000000900ac656363ac51ac52ffffffff93fab89973bd322c5d7ad7e2b929315453e5f7ada3072a36d8e33ca8bebee6e0020000000300acab930da52b04384b04000000000004650052ac435e380200000000076a6a515263ab6aa9494705000000000600ab6a525252af8ba90100000000096565acab526353536a279b17ad",
            "5a59e0b9040654a3596d6dab8146462363cd6549898c26e2476b1f6ae42915f73fd9aedfda00000000036363abffffffff9ac9e9ca90be0187be2214251ff08ba118e6bf5e2fd1ba55229d24e50a510d53010000000165ffffffff41d42d799ac4104644969937522873c0834cc2fcdab7cdbecd84d213c0e96fd60000000000ffffffffd838db2c1a4f30e2eaa7876ef778470f8729fcf258ad228b388df2488709f8410300000000fdf2ace002ceb6d903000000000265654c1310040000000003ac00657e91c0ec",
            "156ebc8202065d0b114984ee98c097600c75c859bfee13af75dc93f57c313a877efb09f230010000000463536a51ffffffff81114e8a697be3ead948b43b5005770dd87ffb1d5ccd4089fa6c8b33d3029e9c03000000066a5251656351ffffffff01a87f140000000000050000ac51ac00000000", 
            "6d97a9a5029220e04f4ccc342d8394c751282c328bf1c132167fc05551d4ca4da4795f6d4e02000000076a0052ab525165ffffffff9516a205e555fa2a16b73e6db6c223a9e759a7e09c9a149a8f376c0a7233fa1b0100000007acab51ab63ac6affffffff04868aed04000000000652ac65ac536a396edf01000000000044386c0000000000076aab5363655200894d48010000000001ab8ebefc23",
            "05921d7c048cf26f76c1219d0237c226454c2a713c18bf152acc83c8b0647a94b13477c07f0300000003ac526afffffffff2f494453afa0cabffd1ba0a626c56f90681087a5c1bd81d6adeb89184b27b7402000000036a6352ffffffff0ad10e2d3ce355481d1b215030820da411d3f571c3f15e8daf22fe15342fed04000000000095f29f7b93ff814a9836f54dc6852ec414e9c4e16a506636715f569151559100ccfec1d100000000055263656a53ffffffff04f4ffef010000000008ac6a6aabacabab6a0e6689040000000006ab536a5352abe364d005000000000965536363655251ab53807e00010000000004526aab63f18003e3",
            "b9b44d9f04b9f15e787d7704e6797d51bc46382190c36d8845ec68dfd63ee64cf7a467b21e00000000096aac00530052ab636aba1bcb110a80c5cbe073f12c739e3b20836aa217a4507648d133a8eedd3f02cb55c132b203000000076a000063526352b1c288e3a9ff1f2da603f230b32ef7c0d402bdcf652545e2322ac01d725d75f5024048ad0100000000ffffffffffd882d963be559569c94febc0ef241801d09dc69527c9490210f098ed8203c700000000056a006300ab9109298d01719d9a0300000000066a52ab006365d7894c5b",
            "ff60473b02574f46d3e49814c484081d1adb9b15367ba8487291fc6714fd6e3383d5b335f001000000026a6ae0b82da3dc77e5030db23d77b58c3c20fa0b70aa7d341a0f95f3f72912165d751afd57230300000008ac536563516a6363ffffffff04f86c0200000000000553acab636ab13111000000000003510065f0d3f305000000000951ab516a65516aabab730a3a010000000002515200000000",
            "f218026204f4f4fc3d3bd0eada07c57b88570d544a0436ae9f8b753792c0c239810bb30fbc0200000002536affffffff8a468928d6ec4cc10aa0f73047697970e99fa64ae8a3b4dca7551deb0b639149010000000851ab520052650051ffffffffa98dc5df357289c9f6873d0f5afcb5b030d629e8f23aa082cf06ec9a95f3b0cf0000000000ffffffffea2c2850c5107705fd380d6f29b03f533482fd036db88739122aac9eff04e0aa010000000365536a03bd37db034ac4c4020000000007515152655200ac33b27705000000000151efb71e0000000000007b65425b",
            "48e7d42103b260b27577b70530d1ac2fed2551e9dd607cbcf66dca34bb8c03862cf8f5fd5401000000075151526aacab00ffffffff1e3d3b841552f7c6a83ee379d9d66636836673ce0b0eda95af8f2d2523c91813030000000665acac006365ffffffff388b3c386cd8c9ef67c83f3eaddc79f1ff910342602c9152ffe8003bce51b28b0100000008636363006a636a52ffffffff04b8f67703000000000852005353ac6552520cef720200000000085151ab6352ab00ab5096d6030000000005516a005100662582020000000001ac6c137280",
            "91ebc4cf01bc1e068d958d72ee6e954b196f1d85b3faf75a521b88a78021c543a06e056279000000000265ab7c12df0503832121030000000000cc41a6010000000005ab5263516540a951050000000006ab63ab65acac00000000",
            "3cd4474201be7a6c25403bf00ca62e2aa8f8f4f700154e1bb4d18c66f7bb7f9b975649f0dc0100000006535151535153ffffffff01febbeb000000000006005151006aac00000000",
            "92fc95f00307a6b3e2572e228011b9c9ed41e58ddbaefe3b139343dbfb3b34182e9fcdc3f50200000002acab847bf1935fde8bcfe41c7dd99683289292770e7f163ad09deff0e0665ed473cd2b56b0f40300000006516551ab6351294dab312dd87b9327ce2e95eb44b712cfae0e50fda15b07816c8282e8365b643390eaab01000000026aacffffffff016e0b6b040000000001ac00000000",
            "4db591ab018adcef5f4f3f2060e41f7829ce3a07ea41d681e8cb70a0e37685561e4767ac3b0000000005000052acabd280e63601ae6ef20000000000036a636326c908f7",
            "c80abebd042cfec3f5c1958ee6970d2b4586e0abec8305e1d99eb9ee69ecc6c2cbd76374380000000007ac53006300ac510acee933b44817db79320df8094af039fd82111c7726da3b33269d3820123694d849ee5001000000056a65ab526562699bea8530dc916f5d61f0babea709dac578774e8a4dcd9c640ec3aceb6cb2443f24f302000000020063ea780e9e57d1e4245c1e5df19b4582f1bf704049c5654f426d783069bcc039f2d8fa659f030000000851ab53635200006a8d00de0b03654e8500000000000463ab635178ebbb0400000000055100636aab239f1d030000000006ab006300536500000000",
            "0337b2d5043eb6949a76d6632b8bb393efc7fe26130d7409ef248576708e2d7f9d0ced9d3102000000075352636a5163007034384dfa200f52160690fea6ce6c82a475c0ef1caf5c9e5a39f8f9ddc1c8297a5aa0eb02000000026a51ffffffff38e536298799631550f793357795d432fb2d4231f4effa183c4e2f61a816bcf0030000000463ac5300706f1cd3454344e521fde05b59b96e875c8295294da5d81d6cc7efcfe8128f150aa54d6503000000008f4a98c704c1561600000000000072cfa6000000000000e43def01000000000100cf31cc0500000000066365526a6500cbaa8e2e",
            "cbc79b10020b15d605680a24ee11d8098ad94ae5203cb6b0589e432832e20c27b72a926af20300000006ab65516a53acbb854f3146e55c508ece25fa3d99dbfde641a58ed88c051a8a51f3dacdffb1afb827814b02000000026352c43e6ef30302410a020000000000ff4bd90100000000065100ab63000008aa8e0400000000095265526565ac5365abc52c8a77",
            "4e8594d803b1d0a26911a2bcdd46d7cbc987b7095a763885b1a97ca9cbb747d32c5ab9aa91030000000353ac53a0cc4b215e07f1d648b6eeb5cdbe9fa32b07400aa773b9696f582cebfd9930ade067b2b200000000060065abab6500fc99833216b8e27a02defd9be47fafae4e4a97f52a9d2a210d08148d2a4e5d02730bcd460100000004516351ac37ce3ae1033baa55040000000006006a636a63acc63c990400000000025265eb1919030000000005656a6a516a00000000",
            "44e1a2b4010762af23d2027864c784e34ef322b6e24c70308a28c8f2157d90d17b99cd94a401000000085163656565006300ffffffff0198233d020000000002000000000000",
            "44ca65b901259245abd50a745037b17eb51d9ce1f41aa7056b4888285f48c6f26cb97b7a25020000000552636363abffffffff047820350400000000040053acab14f3e603000000000652635100ab630ce66c03000000000001bdc704000000000765650065ac51ac3e886381",
            "cfa147d2017fe84122122b4dda2f0d6318e59e60a7207a2d00737b5d89694d480a2c26324b0000000006006351526552ffffffff0456b5b804000000000800516aab525363ab166633000000000004655363ab254c0e02000000000952ab6a6a00ab525151097c1b020000000009656a52ac6300530065ad0d6e50",
            "91c5d5f6022fea6f230cc4ae446ce040d8313071c5ac1749c82982cc1988c94cb1738aa48503000000016a19e204f30cb45dd29e68ff4ae160da037e5fc93538e21a11b92d9dd51cf0b5efacba4dd70000000005656a6aac51ffffffff03db126905000000000953006a53ab6563636a36a273030000000006656a52656552b03ede00000000000352516500000000",
            "03f20dc202c886907b607e278731ebc5d7373c348c8c66cac167560f19b341b782dfb634cb03000000076a51ac6aab63abea3e8de7adb9f599c9caba95aa3fa852e947fc88ed97ee50e0a0ec0d14d164f44c0115c10100000004ab5153516fdd679e0414edbd000000000005ac636a53512021f2040000000007006a0051536a52c73db2050000000005525265ac5369046e000000000003ab006a1ef7bd1e",
            "d9611140036881b61e01627078512bc3378386e1d4761f959d480fdb9d9710bebddba2079d020000000763536aab5153ab819271b41e228f5b04daa1d4e72c8e1955230accd790640b81783cfc165116a9f535a74c000000000163ffffffffa2e7bb9a28e810624c251ff5ba6b0f07a356ac082048cf9f39ec036bba3d431a02000000076a000000ac65acffffffff01678a820000000000085363515153ac635100000000",
            "98b3a0bf034233afdcf0df9d46ac65be84ef839e58ee9fa59f32daaa7d684b6bdac30081c60200000007636351acabababffffffffc71cf82ded4d1593e5825618dc1d5752ae30560ecfaa07f192731d68ea768d0f0100000006650052636563f3a2888deb5ddd161430177ce298242c1a86844619bc60ca2590d98243b5385bc52a5b8f00000000095365acacab520052ac50d4722801c3b8a60300000000035165517e563b65",
            "97be4f7702dc20b087a1fdd533c7de762a3f2867a8f439bddf0dcec9a374dfd0276f9c55cc0300000000cdfb1dbe6582499569127bda6ca4aaff02c132dc73e15dcd91d73da77e92a32a13d1a0ba0200000002ab51ffffffff048cfbe202000000000900516351515363ac535128ce0100000000076aac5365ab6aabc84e8302000000000863536a53ab6a6552f051230500000000066aac535153510848d813",
            "085b6e04040b5bff81e29b646f0ed4a45e05890a8d32780c49d09643e69cdccb5bd81357670100000001abffffffffa5c981fe758307648e783217e3b4349e31a557602225e237f62b636ec26df1a80300000004650052ab4792e1da2930cc90822a8d2a0a91ea343317bce5356b6aa8aae6c3956076aa33a5351a9c0300000004abac5265e27ddbcd472a2f13325cc6be40049d53f3e266ac082172f17f6df817db1936d9ff48c02b000000000152ffffffff021aa7670500000000085353635163ab51ac14d584000000000001aca4d136cc",
            "eec32fff03c6a18b12cd7b60b7bdc2dd74a08977e53fdd756000af221228fe736bd9c42d870100000007005353ac515265ffffffff037929791a188e9980e8b9cc154ad1b0d05fb322932501698195ab5b219488fc02000000070063510065ab6a0bfc176aa7e84f771ea3d45a6b9c24887ceea715a0ff10ede63db8f089e97d927075b4f1000000000551abab63abffffffff02eb933c000000000000262c420000000000036563632549c2b6",
            "3ab70f4604e8fc7f9de395ec3e4c3de0d560212e84a63f8d75333b604237aa52a10da17196000000000763526a6553ac63a25de6fd66563d71471716fe59087be0dde98e969e2b359282cf11f82f14b00f1c0ac70f02000000050052516aacdffed6bb6889a13e46956f4b8af20752f10185838fd4654e3191bf49579c961f5597c36c0100000005ac636363abc3a1785bae5b8a1b4be5d0cbfadc240b4f7acaa7dfed6a66e852835df5eb9ac3c553766801000000036a65630733b7530218569602000000000952006a6a6a51acab52777f06030000000007ac0063530052abc08267c9",
            "bdb6b4d704af0b7234ced671c04ba57421aba7ead0a117d925d7ebd6ca078ec6e7b93eea6600000000026565ffffffff3270f5ad8f46495d69b9d71d4ab0238cbf86cc4908927fbb70a71fa3043108e6010000000700516a65655152ffffffff6085a0fdc03ae8567d0562c584e8bfe13a1bd1094c518690ebcb2b7c6ce5f04502000000095251530052536a53aba576a37f2c516aad9911f687fe83d0ae7983686b6269b4dd54701cb5ce9ec91f0e6828390300000000ffffffff04cc76cc020000000002656a01ffb702000000000253ab534610040000000009acab006565516a00521f55f5040000000000389dfee9",
            "54258edd017d22b274fbf0317555aaf11318affef5a5f0ae45a43d9ca4aa652c6e85f8a040010000000953ac65ab5251656500ffffffff03321d450000000000085265526a51526a529ede8b030000000003635151ce6065020000000001534c56ec1b",
            "ce0d322e04f0ffc7774218b251530a7b64ebefca55c90db3d0624c0ff4b3f03f918e8cf6f60300000003656500ffffffff9cce943872da8d8af29022d0b6321af5fefc004a281d07b598b95f6dcc07b1830200000007abab515351acab8d926410e69d76b7e584aad1470a97b14b9c879c8b43f9a9238e52a2c2fefc2001c56af8010000000400ab5253cd2cd1fe192ce3a93b5478af82fa250c27064df82ba416dfb0debf4f0eb307a746b6928901000000096500abacac6a0063514214524502947efc0200000000035251652c40340100000000096a6aab52000052656a5231c54c",
            "233cd90b043916fc41eb870c64543f0111fb31f3c486dc72457689dea58f75c16ae59e9eb2000000000500536a6a6affffffff9ae30de76be7cd57fb81220fce78d74a13b2dbcad4d023f3cadb3c9a0e45a3ce000000000965ac6353ac5165515130834512dfb293f87cb1879d8d1b20ebad9d7d3d5c3e399a291ce86a3b4d30e4e32368a9020000000453005165ffffffff26d84ae93eb58c81158c9b3c3cbc24a84614d731094f38d0eea8686dec02824d0300000005636a65abacf02c784001a0bd5d03000000000900655351ab65ac516a416ef503",
            "9200e26b03ff36bc4bf908143de5f97d4d02358db642bd5a8541e6ff709c420d1482d471b70000000008abab65536a636553ffffffff61ba6d15f5453b5079fb494af4c48de713a0c3e7f6454d7450074a2a80cb6d880300000007ac6a00ab5165515dfb7574fbce822892c2acb5d978188b1d65f969e4fe874b08db4c791d176113272a5cc10100000000ffffffff0420958d000000000009ac63516a0063516353dd885505000000000465ac00007b79e901000000000066d8bf010000000005525252006a00000000",
            "45f335ba01ce2073a8b0273884eb5b48f56df474fc3dff310d9706a8ac7202cf5ac188272103000000025363ffffffff049d859502000000000365ab6a8e98b1030000000002ac51f3a80603000000000752535151ac00000306e30300000000020051b58b2b3a",
            "94083c840288d40a6983faca876d452f7c52a07de9268ad892e70a81e150d602a773c175ad03000000007ec3637d7e1103e2e7e0c61896cbbf8d7e205b2ecc93dd0d6d7527d39cdbf6d335789f660300000000ffffffff019e1f7b03000000000800ac0051acac0053539cb363",
            "30e0d4d20493d0cd0e640b757c9c47a823120e012b3b64c9c1890f9a087ae4f2001ca22a61010000000152f8f05468303b8fcfaad1fb60534a08fe90daa79bff51675472528ebe1438b6f60e7f60c10100000009526aab6551ac510053ffffffffaaab73957ea2133e32329795221ed44548a0d3a54d1cf9c96827e7cffd1706df0200000009ab00526a005265526affffffffd19a6fe54352015bf170119742821696f64083b5f14fb5c7d1b5a721a3d7786801000000085265abababac53abffffffff020f39bd030000000004ab6aac52049f6c050000000004ab52516aba5b4c60",
            "5c0ac112032d6885b7a9071d3c5f493aa16c610a4a57228b2491258c38de8302014276e8be030000000300ab6a17468315215262ad5c7393bb5e0c5a6429fd1911f78f6f72dafbbbb78f3149a5073e24740300000003ac5100ffffffff33c7a14a062bdea1be3c9c8e973f54ade53fe4a69dcb5ab019df5f3345050be00100000008ac63655163526aab428defc0033ec36203000000000765516365536a00ae55b2000000000002ab53f4c0080400000000095265516a536563536a00000000",
            "e3683329026720010b08d4bec0faa244f159ae10aa582252dd0f3f80046a4e145207d54d31000000000852acac52656aacac3aaf2a5017438ad6adfa3f9d05f53ebed9ceb1b10d809d507bcf75e0604254a8259fc29c020000000653526552ab51f926e52c04b44918030000000000f7679c0100000000090000525152005365539e3f48050000000009516500ab635363ab008396c905000000000253650591024f",
            "00b20fd104dd59705b84d67441019fa26c4c3dec5fd3b50eca1aa549e750ef9ddb774dcabe000000000651ac656aac65ffffffff52d4246f2db568fc9eea143e4d260c698a319f0d0670f84c9c83341204fde48b0200000000ffffffffb8aeabb85d3bcbc67b132f1fd815b451ea12dcf7fc169c1bc2e2cf433eb6777a03000000086a51ac6aab6563acd510d209f413da2cf036a31b0def1e4dcd8115abf2e511afbcccb5ddf41d9702f28c52900100000006ac52ab6a0065ffffffff039c8276000000000008ab53655200656a52401561010000000003acab0082b7160100000000035100ab00000000",
            "624d28cb02c8747915e9af2b13c79b417eb34d2fa2a73547897770ace08c6dd9de528848d3030000000651ab63abab533c69d3f9b75b6ef8ed2df50c2210fd0bf4e889c42477d58682f711cbaece1a626194bb85030000000765acab53ac5353ffffffff018cc280040000000009abacabac52636352ac6859409e",
            "8f28471d02f7d41b2e70e9b4c804f2d90d23fb24d53426fa746bcdcfffea864925bdeabe3e0200000001acffffffff76d1d35d04db0e64d65810c808fe40168f8d1f2143902a1cc551034fd193be0e0000000001acffffffff048a5565000000000005005151516afafb610400000000045263ac53648bb30500000000086363516a6a5165513245de01000000000000000000",
            "10ec50d7046b8b40e4222a3c6449490ebe41513aad2eca7848284a08f3069f3352c2a9954f0000000009526aac656352acac53ffffffff0d979f236155aa972472d43ee6f8ce22a2d052c740f10b59211454ff22cb7fd00200000007acacacab63ab53ffffffffbbf97ebde8969b35725b2e240092a986a2cbfd58de48c4475fe077bdd493a20c010000000663ab5365ababffffffff4600722d33b8dba300d3ad037bcfc6038b1db8abfe8008a15a1de2da2264007302000000035351ac6dbdafaf020d0ccf04000000000663ab6a51ab6ae06e5e0200000000036aabab00000000",
            "43559290038f32fda86580dd8a4bc4422db88dd22a626b8bd4f10f1c9dd325c8dc49bf479f01000000026351ffffffff401339530e1ed3ffe996578a17c3ec9d6fccb0723dd63e7b3f39e2c44b976b7b0300000006ab6a65656a51ffffffff6fb9ba041c96b886482009f56c09c22e7b0d33091f2ac5418d05708951816ce7000000000551ac525100ffffffff020921e40500000000035365533986f40500000000016a00000000",
            "35b6fc06047ebad04783a5167ab5fc9878a00c4eb5e7d70ef297c33d5abd5137a2dea9912402000000036aacacffffffff21dc291763419a584bdb3ed4f6f8c60b218aaa5b99784e4ba8acfec04993e50c03000000046a00ac6affffffff69e04d77e4b662a82db71a68dd72ef0af48ca5bebdcb40f5edf0caf591bb41020200000000b5db78a16d93f5f24d7d932f93a29bb4b784febd0cbb1943f90216dc80bba15a0567684b000000000853ab52ab5100006a1be2208a02f6bdc103000000000265ab8550ea04000000000365636a00000000",
            "f35befbc03faf8c25cc4bc0b92f6239f477e663b44b83065c9cb7cf231243032cf367ce3130000000005ab65526a517c4c334149a9c9edc39e29276a4b3ffbbab337de7908ea6f88af331228bd90086a6900ba020000000151279d19950d2fe81979b72ce3a33c6d82ebb92f9a2e164b6471ac857f3bbd3c0ea213b542010000000953ab51635363520065052657c20300a9ba04000000000452636a6a0516ea020000000008535253656365ababcfdd3f01000000000865ac516aac00530000000000",
            "d3da18520216601acf885414538ce2fb4d910997eeb91582cac42eb6982c9381589587794f0300000000fffffffff1b1c9880356852e10cf41c02e928748dd8fae2e988be4e1c4cb32d0bfaea6f7000000000465ab6aabffffffff02fb0d69050000000002ababeda8580500000000085163526565ac52522b913c95",
            "8218eb740229c695c252e3630fc6257c42624f974bc856b7af8208df643a6c520ef681bfd00000000002510066f30f270a09b2b420e274c14d07430008e7886ec621ba45665057120afce58befca96010300000004525153ab84c380a9015d96100000000000076a5300acac526500000000",
            "1123e7010240310013c74e5def60d8e14dd67aedff5a57d07a24abc84d933483431b8cf8ea0300000003530051fc6775ff1a23c627a2e605dd2560e84e27f4208300071e90f4589e762ad9c9fe8d0da95e020000000465655200ffffffff04251598030000000004ab65ab639d28d90400000000096563636aacac525153474df801000000000851525165ac51006a75e23b040000000000e5bd3a4a",
            "3b937e05032b8895d2f4945cb7e3679be2fbd15311e2414f4184706dbfc0558cf7de7b4d000000000001638b91a12668a3c3ce349788c961c26aa893c862f1e630f18d80e7843686b6e1e6fc396310000000000852635353ab65ac51eeb09dd1c9605391258ee6f74b9ae17b5e8c2ef010dc721c5433dcdc6e93a1593e3b6d1700000000085365ac6553526351ffffffff0308b18e04000000000253acb6dd00040000000008536aac5153ac516ab0a88201000000000500ac006500804e3ff2",
            "a48f27ca047997470da74c8ee086ddad82f36d9c22e790bd6f8603ee6e27ad4d3174ea875403000000095153ac636aab6aacabffffffffefc936294e468d2c9a99e09909ba599978a8c0891ad47dc00ba424761627cef202000000056a51630053ffffffff304cae7ed2d3dbb4f2fbd679da442aed06221ffda9aee460a28ceec5a9399f4e0200000000f5bddf82c9c25fc29c5729274c1ff0b43934303e5f595ce86316fc66ad263b96ca46ab8d0100000003536500d7cf226b0146b00c04000000000200ac5c2014ce",
            "180cd53101c5074cf0b7f089d139e837fe49932791f73fa2342bd823c6df6a2f72fe6dba1303000000076a6a63ac53acabffffffff03853bc1020000000007ac526a6a6a6a003c4a8903000000000453515163a0fbbd030000000005ab656a5253253d64cf",
            "c21ec8b60376c47e057f2c71caa90269888d0ffd5c46a471649144a920d0b409e56f190b700000000008acac6a526a536365ffffffff5d315d9da8bf643a9ba11299450b1f87272e6030fdb0c8adc04e6c1bfc87de9a0000000000ea43a9a142e5830c96b0ce827663af36b23b0277244658f8f606e95384574b91750b8e940000000007516a63ac0063acffffffff023c61be0400000000055165ab5263313cc8020000000006006a53526551ed8c3d56",
            "b8fd394001ed255f49ad491fecc990b7f38688e9c837ccbc7714ddbbf5404f42524e68c18f0000000007ab6353535363ab081e15ee02706f7d050000000008515200535351526364c7ec040000000005636a53acac9206cbe1",
            "e42a76740264677829e30ed610864160c7f97232c16528fe5610fc08814b21c34eefcea69d010000000653006a6a0052ffffffff647046cf44f217d040e6a8ff3f295312ab4dd5a0df231c66968ad1c6d8f4428000000000025352ffffffff0199a7f900000000000000000000",
            "a2dfa4690214c1ab25331815a5128f143219de51a47abdc7ce2d367e683eeb93960a31af9f010000000363636affffffff8be0628abb1861b078fcc19c236bc4cc726fa49068b88ad170adb2a97862e7460200000004ac655363ffffffff0441f11103000000000153dbab0c000000000009ab53ac5365526aab63abbb95050000000004ab52516a29a029040000000003ac526a00000000",
            "9dbc591f04521670af83fb3bb591c5d4da99206f5d38e020289f7db95414390dddbbeb56680100000004ac5100acffffffffb6a40b5e29d5e459f8e72d39f800089529f0889006cad3d734011991da8ef09d0100000009526a5100acab536a515fc427436df97cc51dc8497642ffc868857ee245314d28b356bd70adba671bd6071301fc0000000000ffffffff487efde2f620566a9b017b2e6e6d42525e4070f73a602f85c6dfd58304518db30000000005516353006a8d8090180244904a0200000000046a65656ab1e9c203000000000451ab63aba06a5449",
            "1345fb2c04bb21a35ae33a3f9f295bece34650308a9d8984a989dfe4c977790b0c21ff9a7f0000000006ac52ac6a0053ffffffff7baee9e8717d81d375a43b691e91579be53875350dfe23ba0058ea950029fcb7020000000753ab53ab63ab52ffffffff684b6b3828dfb4c8a92043b49b8cb15dd3a7c98b978da1d314dce5b9570dadd202000000086353ab6a5200ac63d1a8647bf667ceb2eae7ec75569ca249fbfd5d1b582acfbd7e1fcf5886121fca699c011d0100000003ac006affffffff049b1eb00300000000001e46dc0100000000080065ab6a6a630065ca95b40300000000030051520c8499010000000006ab6aac526a6500000000",
            "7d75dc8f011e5f9f7313ba6aedef8dbe10d0a471aca88bbfc0c4a448ce424a2c5580cda1560300000003ab5152ffffffff01997f8e0200000000096552ac6a65656563530d93bbcc",
            "1459179504b69f01c066e8ade5e124c748ae5652566b34ed673eea38568c483a5a4c4836ca0100000008ac5352006563656affffffff5d4e037880ab1975ce95ea378d2874dcd49d5e01e1cdbfae3343a01f383fa35800000000095251ac52ac6aac6500ffffffff7de3ae7d97373b7f2aeb4c55137b5e947b2d5fb325e892530cb589bc4f92abd503000000086563ac53ab520052ffffffffb4db36a32d6e543ef49f4bafde46053cb85b2a6c4f0e19fa0860d9083901a1190300000003ab51531bbcfe5504a6dbda040000000008536a5365abac6500d660c80300000000096565abab6a53536a6a54e84e010000000003acac52df2ccf0500000000025351220c857e",
            "cabb1b06045a895e6dcfc0c1e971e94130c46feace286759f69a16d298c8b0f6fd0afef8f20300000004ac006352ffffffffa299f5edac903072bfb7d29b663c1dd1345c2a33546a508ba5cf17aab911234602000000056a65515365ffffffff89a20dc2ee0524b361231092a070ace03343b162e7162479c96b757739c8394a0300000002abab92ec524daf73fabee63f95c1b79fa8b84e92d0e8bac57295e1d0adc55dc7af5534ebea410200000001534d70e79b04674f6f00000000000600abacab53517d60cc0200000000035265ab96c51d040000000004ac6300ac62a787050000000008006a516563ab63639e2e7ff7",
            "8b96d7a30132f6005b5bd33ea82aa325e2bcb441f46f63b5fca159ac7094499f380f6b7e2e00000000076aacabac6300acffffffff0158056700000000000465005100c319e6d0",
            "112191b7013cfbe18a175eaf09af7a43cbac2c396f3695bbe050e1e5f4250603056d60910e02000000001c8a5bba03738a22010000000005525352656a77a149010000000002510003b52302000000000351ac52722be8e6",
            "ce6e1a9e04b4c746318424705ea69517e5e0343357d131ad55d071562d0b6ebfedafd6cb840100000003656553ffffffff67bd2fa78e2f52d9f8900c58b84c27ef9d7679f67a0a6f78645ce61b883fb8de000000000100d699a56b9861d99be2838e8504884af4d30b909b1911639dd0c5ad47c557a0773155d4d303000000046a5151abffffffff9fdb84b77c326921a8266854f7bbd5a71305b54385e747fe41af8a397e78b7fa010000000863acac6a51ab00ac0d2e9b9d049b8173010000000007ac53526a650063ba9b7e010000000008526a00525263acac0ab3fd030000000000ea8a0303000000000200aca61a97b9",
            "2f7353dd02e395b0a4d16da0f7472db618857cd3de5b9e2789232952a9b154d249102245fd030000000151617fd88f103280b85b0a198198e438e7cab1a4c92ba58409709997cc7a65a619eb9eec3c0200000003636aabffffffff0397481c0200000000045300636a0dc97803000000000009d389030000000003ac6a53134007bb",
            "89e7928c04363cb520eff4465251fd8e41550cbd0d2cdf18c456a0be3d634382abcfd4a2130200000006ac516a6a656355042a796061ed72db52ae47d1607b1ceef6ca6aea3b7eea48e7e02429f382b378c4e51901000000085351ab6352ab5252ffffffff53631cbda79b40183000d6ede011c778f70147dc6fa1aed3395d4ce9f7a8e69701000000096a6553ab52516a52abad0de418d80afe059aab5da73237e0beb60af4ac490c3394c12d66665d1bac13bdf29aa8000000000153f2b59ab6027a33eb040000000007005351ac5100ac88b941030000000003ab0052e1e8a143",
            "ca356e2004bea08ec2dd2df203dc275765dc3f6073f55c46513a588a7abcc4cbde2ff011c7020000000553525100003aefec4860ef5d6c1c6be93e13bd2d2a40c6fb7361694136a7620b020ecbaca9413bcd2a030000000965ac00536352535100ace4289e00e97caaea741f2b89c1143060011a1f93090dc230bee3f05e34fbd8d8b6c399010000000365526affffffff48fc444238bda7a757cb6a98cb89fb44338829d3e24e46a60a36d4e24ba05d9002000000026a53ffffffff03d70b440200000000056a6a526aac853c97010000000002515335552202000000000351635300000000",
            "82d4fa65017958d53e562fac073df233ab154bd0cf6e5a18f57f4badea8200b217975e31030200000004636aab51ac0891a204227cc9050000000006635200655365bfef8802000000000865650051635252acfc2d09050000000006ab65ac51516380195e030000000007ac52525352510063d50572",
            "75f6949503e0e47dd70426ef32002d6cdb564a45abedc1575425a18a8828bf385fa8e808e600000000036aabab82f9fd14e9647d7a1b5284e6c55169c8bd228a7ea335987cef0195841e83da45ec28aa2e0300000002516350dc6fe239d150efdb1b51aa288fe85f9b9f741c72956c11d9dcd176889963d699abd63f0000000001ab429a63f502777d20010000000007abac52ac516a53d081d9020000000003acac630c3cc3a8",
            "e86a24bc03e4fae784cdf81b24d120348cb5e52d937cd9055402fdba7e43281e482e77a1c100000000046363006affffffffa5447e9bdcdab22bd20d88b19795d4c8fb263fbbf7ce8f4f9a85f865953a6325020000000663ac53535253ffffffff9f8b693bc84e0101fc73748e0513a8cecdc264270d8a4ee1a1b6717607ee1eaa00000000026a513417bf980158d82c020000000009005253005351acac5200000000",
            "536bc5e60232eb60954587667d6bcdd19a49048d67a027383cc0c2a29a48b960dc38c5a0370300000005ac636300abffffffff8f1cfc102f39b1c9348a2195d496e602c77d9f57e0769dabde7eaaedf9c69e250100000006acabab6a6351ffffffff0432f56f0400000000046a5365517fd54b0400000000035265539484e4050000000003536a5376dc25020000000008ac536aab6aab536ab978e686",
            "fab796ee03f737f07669160d1f1c8bf0800041157e3ac7961fea33a293f976d79ce49c02ab0200000003ac5252eb097ea1a6d1a7ae9dace338505ba559e579a1ee98a2e9ad96f30696d6337adcda5a85f403000000096500abab656a6a656396d5d41a9b11f571d91e4242ddc0cf2420eca796ad4882ef1251e84e42b930398ec69dd80100000005526551ac6a8e5d0de804f763bb0400000000015288271a010000000001acf2bf2905000000000300ab51c9641500000000000952655363636365ac5100000000",
            "f2b539a401e4e8402869d5e1502dbc3156dbce93583f516a4947b333260d5af1a34810c6a00200000003525363ffffffff01d305e2000000000005acab535200a265fe77",
            "9f10b1d8033aee81ac04d84ceee0c03416a784d1017a2af8f8a34d2f56b767aea28ff88c8f02000000025352ffffffff748cb29843bea8e9c44ed5ff258df1faf55fbb9146870b8d76454786c4549de100000000016a5ba089417305424d05112c0ca445bc7107339083e7da15e430050d578f034ec0c589223b0200000007abac53ac6565abffffffff025a4ecd010000000006636563ab65ab40d2700000000000056a6553526333fa296c",
            "ff2ecc09041b4cf5abb7b760e910b775268abee2792c7f21cc5301dd3fecc1b4233ee70a2c0200000009acac5300006a51526affffffffeb39c195a5426afff38379fc85369771e4933587218ef4968f3f05c51d6b7c92000000000165453a5f039b8dbef7c1ffdc70ac383b481f72f99f52b0b3a5903c825c45cfa5d2c0642cd50200000001654b5038e6c49daea8c0a9ac8611cfe904fc206dad03a41fb4e5b1d6d85b1ecad73ecd4c0102000000096a51000053ab656565bdb5548302cc719200000000000452655265214a3603000000000300ab6a00000000",
            "70a8577804e553e462a859375957db68cfdf724d68caeacf08995e80d7fa93db7ebc04519d02000000045352ab53619f4f2a428109c5fcf9fee634a2ab92f4a09dc01a5015e8ecb3fc0d9279c4a77fb27e900000000006ab6a51006a6affffffff3ed1a0a0d03f25c5e8d279bb5d931b7eb7e99c8203306a6c310db113419a69ad010000000565516300abffffffff6bf668d4ff5005ef73a1b0c51f32e8235e67ab31fe019bf131e1382050b39a630000000004536a6563ffffffff02faf0bb00000000000163cf2b4b05000000000752ac635363acac15ab369f",
            "a3604e5304caa5a6ba3c257c20b45dcd468f2c732a8ca59016e77b6476ac741ce8b16ca8360200000004acac6553ffffffff695e7006495517e0b79bd4770f955040610e74d35f01e41c9932ab8ccfa3b55d0300000007ac5253515365acffffffff6153120efc5d73cd959d72566fc829a4eb00b3ef1a5bd3559677fb5aae116e38000000000400abab52c29e7abd06ff98372a3a06227386609adc7665a602e511cadcb06377cc6ac0b8f63d4fdb03000000055100acabacffffffff04209073050000000009ab5163ac525253ab6514462e05000000000952abacab636300656a20672c0400000000025153b276990000000000056565ab6a5300000000",
            "c6a72ed403313b7d027f6864e705ec6b5fa52eb99169f8ea7cd884f5cdb830a150cebade870100000009ac63ab516565ab6a51ffffffff398d5838735ff43c390ca418593dbe43f3445ba69394a6d665b5dc3b4769b5d700000000075265acab515365ffffffff7ee5616a1ee105fd18189806a477300e2a9cf836bf8035464e8192a0d785eea3030000000700ac6a51516a52ffffffff018075fd0000000000015100000000",
            "93c12cc30270fc4370c960665b8f774e07942a627c83e58e860e38bd6b0aa2cb7a2c1e060901000000036300abffffffff4d9b618035f9175f564837f733a2b108c0f462f28818093372eec070d9f0a5440300000001acffffffff039c2137020000000001525500990100000000055265ab636a07980e0300000000005ba0e9d1",
            "97bddc63015f1767619d56598ad0eb5c7e9f880b24a928fea1e040e95429c930c1dc653bdb0100000008ac53acac00005152aaa94eb90235ed10040000000000287bdd0400000000016a8077673a",
            "cc3c9dd303637839fb727270261d8e9ddb8a21b7f6cbdcf07015ba1e5cf01dc3c3a327745d0300000000d2d7804fe20a9fca9659a0e49f258800304580499e8753046276062f69dbbde85d17cd2201000000096352536a520000acabffffffffbc75dfa9b5f81f3552e4143e08f485dfb97ae6187330e6cd6752de6c21bdfd21030000000600ab53650063ffffffff0313d0140400000000096565515253526aacac167f0a040000000008acab00535263536a9a52f8030000000006abab5151ab63f75b66f2",
            "236f91b702b8ffea3b890700b6f91af713480769dda5a085ae219c8737ebae90ff25915a3203000000056300ac6300811a6a10230f12c9faa28dae5be2ebe93f37c06a79e76214feba49bb017fb25305ff84eb020000000100ffffffff041e351703000000000351ac004ff53e050000000003ab53636c1460010000000000cb55f701000000000651520051ab0000000000",
            "c47d5ad60485cb2f7a825587b95ea665a593769191382852f3514a486d7a7a11d220b62c54000000000663655253acab8c3cf32b0285b040e50dcf6987ddf7c385b3665048ad2f9317b9e0c5ba0405d8fde4129b00000000095251ab00ac65635300ffffffff549fe963ee410d6435bb2ed3042a7c294d0c7382a83edefba8582a2064af3265000000000152fffffffff7737a85e0e94c2d19cd1cde47328ece04b3e33cd60f24a8a345da7f2a96a6d0000000000865ab6a0051656aab28ff30d5049613ea020000000005ac51000063f06df1050000000008ac63516aabac5153afef5901000000000700656500655253688bc00000000000086aab5352526a53521ff1d5ff",
            "0b43f122032f182366541e7ee18562eb5f39bc7a8e5e0d3c398f7e306e551cdef773941918030000000863006351ac51acabffffffffae586660c8ff43355b685dfa8676a370799865fbc4b641c5a962f0849a13d8250100000005abab63acabffffffff0b2b6b800d8e77807cf130de6286b237717957658443674df047a2ab18e413860100000008ab6aac655200ab63ffffffff04f1dbca03000000000800635253ab656a52a6eefd0300000000036365655d8ca90200000000005a0d530400000000015300000000",
            "af1c4ab301ec462f76ee69ba419b1b2557b7ded639f3442a3522d4f9170b2d6859765c3df402000000016affffffff01a5ca6c000000000008ab52536aab00005300000000",
            "0bfd34210451c92cdfa02125a62ba365448e11ff1db3fb8bc84f1c7e5615da40233a8cd368010000000252ac9a070cd88dec5cf9aed1eab10d19529720e12c52d3a21b92c6fdb589d056908e43ea910e0200000009ac516a52656a6a5165ffffffffc3edcca8d2f61f34a5296c405c5f6bc58276416c720c956ff277f1fb81541ddd00000000030063abffffffff811247905cdfc973d179c03014c01e37d44e78f087233444dfdce1d1389d97c302000000065163000063ab1724a26e02ca37c902000000000851ab53525352ac529012a90100000000085200525253535353fa32575b",
            "467a3e7602e6d1a7a531106791845ec3908a29b833598e41f610ef83d02a7da3a1900bf2960000000005ab6a636353ffffffff031db6dac6f0bafafe723b9199420217ad2c94221b6880654f2b35114f44b1df010000000965ab52636a63ac6352ffffffff02b3b95c0100000000026300703216030000000001ab3261c0aa",
            "8713bc4f01b411149d575ebae575f5dd7e456198d61d238695df459dd9b86c4e3b2734b62e0300000004abac6363ffffffff03b58049050000000002ac653c714c04000000000953656a005151526a527b5a9e03000000000652ac5100525300000000",
            "b5a7df6102107beded33ae7f1dec0531d4829dff7477260925aa2cba54119b7a07d92d5a1d02000000046a516a52803b625c334c1d2107a326538a3db92c6c6ae3f7c3516cd90a09b619ec6f58d10e77bd6703000000056563006a63ffffffff0117484b03000000000853acab52526a65abc1b548a1",
            "49eb2178020a04fca08612c34959fd41447319c190fb7ffed9f71c235aa77bec28703aa1820200000003ac6353abaff326071f07ec6b77fb651af06e8e8bd171068ec96b52ed584de1d71437fed186aecf0300000001acffffffff03da3dbe02000000000652ac63ac6aab8f3b680400000000096a536a65636a53516a5175470100000000016500000000",
            "0f96cea9019b4b3233c0485d5b1bad770c246fe8d4a58fb24c3b7dfdb3b0fd90ea4e8e947f0300000006006a5163515303571e1e01906956030000000005ab635353abadc0fbbe",
            "148e68480196eb52529af8e83e14127cbfdbd4a174e60a86ac2d86eac9665f46f4447cf7aa01000000045200ac538f8f871401cf240c0300000000065252ab52656a5266cf61",
            "6c7913f902aa3f5f939dd1615114ce961beda7c1e0dd195be36a2f0d9d047c28ac62738c3a020000000453abac00ffffffff477bf2c5b5c6733881447ac1ecaff3a6f80d7016eee3513f382ad7f554015b970100000007ab6563acab5152ffffffff04e58fe1040000000009ab00526aabab526553e59790010000000002ab525a834b03000000000035fdaf0200000000086551ac65515200ab00000000",
            "3320f6730132f830c4681d0cae542188e4177cad5d526fae84565c60ceb5c0118e844f90bd030000000163ffffffff0257ec5a040000000005525251ac6538344d000000000002515200000000",
            "c13aa4b702eedd7cde09d0416e649a890d40e675aa9b5b6d6912686e20e9b9e10dbd40abb1000000000863ab6353515351ac11d24dc4cc22ded7cdbc13edd3f87bd4b226eda3e4408853a57bcd1becf2df2a1671fd1600000000045165516affffffff01baea300100000000076aab52ab53005300000000",
            "a2692fff03b2387f5bacd5640c86ba7df574a0ee9ed7f66f22c73cccaef3907eae791cbd230200000004536363abffffffff4d9fe7e5b375de88ba48925d9b2005447a69ea2e00495a96eafb2f144ad475b40000000008000053000052636537259bee3cedd3dcc07c8f423739690c590dc195274a7d398fa196af37f3e9b4a1413f810000000006ac63acac52abffffffff04c65fe60200000000075151536365ab657236fc020000000009005263ab00656a6a5195b8b6030000000007ac5165636aac6a7d7b66010000000002acab00000000",
            "2c5b003201b88654ac2d02ff6762446cb5a4af77586f05e65ee5d54680cea13291efcf930d0100000005ab536a006a37423d2504100367000000000004536a515335149800000000000152166aeb03000000000452510063226c8e03000000000000000000",
            "f981b9e104acb93b9a7e2375080f3ea0e7a94ce54cd8fb25c57992fa8042bdf4378572859f0100000002630008604febba7e4837da77084d5d1b81965e0ea0deb6d61278b6be8627b0d9a2ecd7aeb06a0300000005ac5353536a42af3ef15ce7a2cd60482fc0d191c4236e66b4b48c9018d7dbe4db820f5925aad0e8b52a0300000008ab0063510052516301863715efc8608bf69c0343f18fb81a8b0c720898a3563eca8fe630736c0440a179129d03000000086aac6a52ac6a63ac44fec4c00408320a03000000000062c21c030000000007ac6a655263006553835f0100000000015303cd60000000000005535263536558b596e0",
            "c4b702e502f1a54f235224f0e6de961d2e53b506ab45b9a40805d1dacd35148f0acf24ca5e00000000085200ac65ac53acabf34ba6099135658460de9d9b433b84a8562032723635baf21ca1db561dce1c13a06f4407000000000851ac006a63516aabffffffff02a853a603000000000163d17a67030000000005ab63006a5200000000",
            "9b83f78704f492b9b353a3faad8d93f688e885030c274856e4037818848b99e490afef27770200000000ffffffff36b60675a5888c0ef4d9e11744ecd90d9fe9e6d8abb4cff5666c898fdce98d9e00000000056aab656352596370fca7a7c139752971e169a1af3e67d7656fc4fc7fd3b98408e607c2f2c836c9f27c030000000653ac51ab6300a0761de7e158947f401b3595b7dc0fe7b75fa9c833d13f1af57b9206e4012de0c41b8124030000000953656a53ab53510052242e5f5601bf83b301000000000465516a6300000000",
            "f492a9da04f80b679708c01224f68203d5ea2668b1f442ebba16b1aa4301d2fe5b4e2568f3010000000953005351525263ab65ffffffff93b34c3f37d4a66df255b514419105b56d7d60c24bf395415eda3d3d8aa5cd0101000000020065ffffffff9dba34dabdc4f1643b372b6b77fdf2b482b33ed425914bb4b1a61e4fad33cf390000000002ab52ffffffffbbf3dc82f397ef3ee902c5146c8a80d9a1344fa6e38b7abce0f157be7adaefae0000000009515351005365006a51ffffffff021359ba010000000000403fea0200000000095200ac6353abac635300000000",
            "2f73e0b304f154d3a00fde2fdd40e791295e28d6cb76af9c0fd8547acf3771a02e3a92ba37030000000852ac6351ab6565639aa95467b065cec61b6e7dc4d6192b5536a7c569315fb43f470078b31ed22a55dab8265f02000000080065636a6aab6a53ffffffff9e3addbff52b2aaf9fe49c67017395198a9b71f0aa668c5cb354d06c295a691a0100000000ffffffff45c2b4019abaf05c5e484df982a4a07459204d1343a6ee5badade358141f8f990300000007ac516a6aacac6308655cd601f3bc2f0000000000015200000000",
            "5a60b9b503553f3c099f775db56af3456330f1e44e67355c4ab290d22764b9144a7b5f959003000000030052acbd63e0564decc8659aa53868be48c1bfcda0a8c9857b0db32a217bc8b46d9e7323fe9649020000000553ac6551abd0ecf806211db989bead96c09c7f3ec5f73c1411d3329d47d12f9e46678f09bac0dc383e0200000000ffffffff01494bb202000000000500516551ac00000000",
            "e3649aa40405e6ffe377dbb1bbbb672a40d8424c430fa6512c6165273a2b9b6afa9949ec430200000007630052ab655153a365f62f2792fa90c784efe3f0981134d72aac0b1e1578097132c7f0406671457c332b84020000000353ab6ad780f40cf51be22bb4ff755434779c7f1def4999e4f289d2bd23d142f36b66fbe5cfbb4b01000000076a5252abac52ab1430ffdc67127c9c0fc97dcd4b578dab64f4fb9550d2b59d599773962077a563e8b6732c02000000016affffffff04cb2687000000000002ab636e320904000000000252acf70e9401000000000100dc3393050000000006ab0063536aacbc231765",
            "4c4be7540344050e3044f0f1d628039a334a7c1f7b4573469cfea46101d6888bb6161fe9710200000000ffffffffac85a4fdad641d8e28523f78cf5b0f4dc74e6c5d903c10b358dd13a5a1fd8a06000000000163e0ae75d05616b72467b691dc207fe2e65ea35e2eadb7e06ea442b2adb9715f212c0924f10200000000ffffffff0194ddfe02000000000265ac00000000",
            "202c18eb012bc0a987e69e205aea63f0f0c089f96dd8f0e9fcde199f2f37892b1d4e6da90302000000055352ac6565ffffffff0257e5450100000000025300ad257203000000000000000000",
            "32fa0b0804e6ea101e137665a041cc2350b794e59bf42d9b09088b01cde806ec1bbea077df0200000008515153650000006506a11c55904258fa418e57b88b12724b81153260d3f4c9f080439789a391ab147aabb0fa0000000007000052ac51ab510986f2a15c0d5e05d20dc876dd2dafa435276d53da7b47c393f20900e55f163b97ce0b800000000008ab526a520065636a8087df7d4d9c985fb42308fb09dce704650719140aa6050e8955fa5d2ea46b464a333f870000000009636300636a6565006affffffff01994a0d040000000002536500000000",
            "ae23424d040cd884ebfb9a815d8f17176980ab8015285e03fdde899449f4ae71e04275e9a80100000007ab006553530053ffffffff018e06db6af519dadc5280c07791c0fd33251500955e43fe4ac747a4df5c54df020000000251ac330e977c0fec6149a1768e0d312fdb53ed9953a3737d7b5d06aad4d86e9970346a4feeb5030000000951ab51ac6563ab526a67cabc431ee3d8111224d5ecdbb7d717aa8fe82ce4a63842c9bd1aa848f111910e5ae1eb0100000004ac515300bfb7e0d7048acddc030000000009636a5253636a655363a3428e040000000001525b99c6050000000004655265ab717e6e020000000000d99011eb",
            "030f44fc01b4a9267335a95677bd190c1c12655e64df74addc53b753641259af1a54146baa020000000152e004b56c04ba11780300000000026a53f125f001000000000251acd2cc7c03000000000763536563655363c9b9e50500000000015200000000",
            "c05f448f02817740b30652c5681a3b128322f9dc97d166bd4402d39c37c0b14506d8adb5890300000003536353ffffffffa188b430357055ba291c648f951cd2f9b28a2e76353bef391b71a889ba68d5fc02000000056565526a6affffffff02745f73010000000001ab3ec34c0400000000036aac5200000000",
            "163ba45703dd8c2c5a1c1f8b806afdc710a2a8fc40c0138e2d83e329e0e02a9b6c837ff6b8000000000700655151ab6a522b48b8f134eb1a7e6f5a6fa319ce9d11b36327ba427b7d65ead3b4a6a69f85cda8bbcd22030000000563656552acffffffffdbcf4955232bd11eef0cc6954f3f6279675b2956b9bcc24f08c360894027a60201000000066500006500abffffffff04d0ce9d0200000000008380650000000000015233f360040000000003006aabedcf0801000000000000000000",
            "fe647f950311bf8f3a4d90afd7517df306e04a344d2b2a2fea368935faf11fa6882505890d0000000005ab5100516affffffff43c140947d9778718919c49c0535667fc6cc727f5876851cb8f7b6460710c7f60100000000ffffffffce4aa5d90d7ab93cbec2e9626a435afcf2a68dd693c15b0e1ece81a9fcbe025e0300000000ffffffff02f34806020000000002515262e54403000000000965635151ac655363636de5ce24",
            "cef7316804c3e77fe67fc6207a1ea6ae6eb06b3bf1b3a4010a45ae5c7ad677bb8a4ebd16d90200000009ac536a5152ac5263005301ab8a0da2b3e0654d31a30264f9356ba1851c820a403be2948d35cafc7f9fe67a06960300000006526a63636a53ffffffffbada0d85465199fa4232c6e4222df790470c5b7afd54704595a48eedd7a4916b030000000865ab63ac006a006ab28dba4ad55e58b5375053f78b8cdf4879f723ea4068aed3dd4138766cb4d80aab0aff3d0300000003ac6a00ffffffff010f5dd6010000000006ab006aab51ab00000000",
            "7b3ff28004ba3c7590ed6e36f45453ebb3f16636fe716acb2418bb2963df596a50ed954d2e03000000065251515265abffffffff706ee16e32e22179400c9841013971645dabf63a3a6d2d5feb42f83aa468983e030000000653ac51ac5152ffffffffa03a16e5e5de65dfa848b9a64ee8bf8656cc1f96b06a15d35bd5f3d32629876e020000000043c1a3965448b3b46f0f0689f1368f3b2981208a368ec5c30defb35595ef9cf95ffd10e902000000036aac65253a5bbe042e907204000000000800006565656352634203b4020000000002656336b3b7010000000001ab7a063f0100000000026500a233cb76",
            "1be8ee5604a9937ebecffc832155d9ba7860d0ca451eaced58ca3688945a31d93420c27c460100000006abac5300535288b65458af2f17cbbf7c5fbcdcfb334ffd84c1510d5500dc7d25a43c36679b702e850f7c0200000003005300ffffffff7c237281cb859653eb5bb0a66dbb7aeb2ac11d99ba9ed0f12c766a8ae2a2157203000000086aabac526365acabfffffffff09d3d6639849f442a6a52ad10a5d0e4cb1f4a6b22a98a8f442f60280c9e5be80200000007ab00ab6565ab52ffffffff0398fe83030000000005526aababacbdd6ec010000000005535252ab6a82c1e6040000000001652b71c40c",
            "9e0f99c504fbca858c209c6d9371ddd78985be1ab52845db0720af9ae5e2664d352f5037d4010000000552ac53636affffffff0e0ce866bc3f5b0a49748f597c18fa47a2483b8a94cef1d7295d9a5d36d31ae7030000000663515263ac635bb5d1698325164cdd3f7f3f7831635a3588f26d47cc30bf0fefd56cd87dc4e84f162ab702000000036a6365ffffffff85c2b1a61de4bcbd1d5332d5f59f338dd5e8accbc466fd860f96eef1f54c28ec030000000165ffffffff04f5cabd010000000007000052ac526563c18f1502000000000465510051dc9157050000000008655363ac525253ac506bb600000000000865656a53ab63006a00000000",
            "11ce51f90164b4b54b9278f0337d95c50d16f6828fcb641df9c7a041a2b274aa70b1250f2b0000000008ab6a6a65006551524c9fe7f604af44be050000000005525365006521f79a0300000000015306bb4e04000000000265ac99611a05000000000765acab656500006dc866d0",
            "86bc233e02ba3c647e356558e7252481a7769491fb46e883dd547a4ce9898fc9a1ca1b77790000000006ab5351abab51f0c1d09c37696d5c7c257788f5dff5583f4700687bcb7d4acfb48521dc953659e325fa390300000003acac5280f29523027225af03000000000963abac0065ab65acab7e59d90400000000016549dac846",
            "beac155d03a853bf18cd5c490bb2a245b3b2a501a3ce5967945b0bf388fec2ba9f04c03d68030000000012fe96283aec4d3aafed8f888b0f1534bd903f9cd1af86a7e64006a2fa0d2d30711af770010000000163ffffffffd963a19d19a292104b9021c535d3e302925543fb3b5ed39fb2124ee23a9db00302000000056500ac63acffffffff01ad67f503000000000300ac5189f78db2",
            "489ebbf10478e260ba88c0168bd7509a651b36aaee983e400c7063da39c93bf28100011f280100000004abab63ab2fc856f05f59b257a4445253e0d91b6dffe32302d520ac8e7f6f2467f7f6b4b65f2f59e903000000096353abacab6351656affffffff0122d9480db6c45a2c6fd68b7bc57246edffbf6330c39ccd36aa3aa45ec108fc030000000265ab9a7e78a69aadd6b030b12602dff0739bbc346b466c7c0129b34f50ae1f61e634e11e9f3d0000000006516a53525100ffffffff011271070000000000086563ab6353536352c4dd0e2c",
            "6911195d04f449e8eade3bc49fd09b6fb4b7b7ec86529918b8593a9f6c34c2f2d301ec378b000000000263ab49162266af054643505b572c24ff6f8e4c920e601b23b3c42095881857d00caf56b28acd030000000565525200ac3ac4d24cb59ee8cfec0950312dcdcc14d1b360ab343e834004a5628d629642422f3c5acc02000000035100accf99b663e3c74787aba1272129a34130668a877cc6516bfb7574af9fa6d07f9b4197303400000000085351ab5152635252ffffffff042b3c95000000000000ff92330200000000046a5252ab884a2402000000000853530065520063000d78be03000000000953abab52ab53ac65aba72cb34b",
            "746347cf03faa548f4c0b9d2bd96504d2e780292730f690bf0475b188493fb67ca58dcca4f0000000002005336e3521bfb94c254058e852a32fc4cf50d99f9cc7215f7c632b251922104f638aa0b9d080100000008656aac5351635251ffffffff4da22a678bb5bb3ad1a29f97f6f7e5b5de11bb80bcf2f7bb96b67b9f1ac44d09030000000365ababffffffff036f02b30000000000076353ab6aac63ac50b72a050000000002acaba8abf804000000000663006a6a6353797eb999",
            "e17149010239dd33f847bf1f57896db60e955117d8cf013e7553fae6baa9acd3d0f1412ad90200000006516500516500cb7b32a8a67d58dddfb6ceb5897e75ef1c1ff812d8cd73875856487826dec4a4e2d2422a0100000004ac525365196dbb69039229270400000000070000535351636a8b7596020000000006ab51ac52655131e99d040000000003516551ee437f5c",
            "144971940223597a2d1dec49c7d4ec557e4f4bd207428618bafa3c96c411752d494249e1fb0100000004526a5151ffffffff340a545b1080d4f7e2225ff1c9831f283a7d4ca4d3d0a29d12e07d86d6826f7f0200000003006553ffffffff03c36965000000000000dfa9af00000000000451636aac7f7d140300000000016300000000",
            "2aee6b9a02172a8288e02fac654520c9dd9ab93cf514d73163701f4788b4caeeb9297d2e250300000004ab6363008fb36695528d7482710ea2926412f877a3b20acae31e9d3091406bfa6b62ebf9d9d2a6470100000009535165536a63520065ffffffff03f7b560050000000003acab6a9a8338050000000000206ce90000000000056552516a5100000000",
            "9554595203ad5d687f34474685425c1919e3d2cd05cf2dac89d5f33cd3963e5bb43f8706480100000000ffffffff9de2539c2fe3000d59afbd376cb46cefa8bd01dbc43938ff6089b63d68acdc2b02000000096553655251536a6500fffffffff9695e4016cd4dfeb5f7dadf00968e6a409ef048f81922cec231efed4ac78f5d010000000763abab6a5365006caaf0070162cc640200000000045163ab5100000000",
            "04f51f2a0484cba53d63de1cb0efdcb222999cdf2dd9d19b3542a896ca96e23a643dfc45f00200000007acac53510063002b091fd0bfc0cfb386edf7b9e694f1927d7a3cf4e1d2ce937c1e01610313729ef6419ae7030000000165a3372a913c59b8b3da458335dc1714805c0db98992fd0d93f16a7f28c55dc747fe66a5b503000000095351ab65ab52536351ffffffff5650b318b3e236802a4e41ed9bc0a19c32b7aa3f9b2cda1178f84499963a0cde000000000165ffffffff0383954f04000000000553ac536363a8fc90030000000000a2e315000000000005acab00ab5100000000",
            "53dc1a88046531c7b57a35f4d9adf101d068bf8d63fbbedaf4741dba8bc5e92c8725def571030000000453655251fcdf116a226b3ec240739c4c7493800e4edfe67275234e371a227721eac43d3d9ecaf1b50300000003ac0052ffffffff2c9279ffeea4718d167e9499bd067600715c14484e373ef93ae4a31d2f5671ab0000000009516553ac636a6a65001977752eeba95a8f16b88c571a459c2f2a204e23d48cc7090e4f4cc35846ca7fc0a455ce00000000055165ac0063188143f80205972902000000000765ac63ac516353c7b6a50000000000036a510000000000",
            "5a06cb4602dcfc85f49b8d14513f33c48f67146f2ee44959bbca092788e6823b2719f3160b0200000001ab3c013f2518035b9ea635f9a1c74ec1a3fb7496a160f46aae2e09bfc5cd5111a0f20969e003000000015158c89ab7049f20d6010000000008ac6a52abac53515349765e00000000000300ab638292630100000000045351ab0086da09010000000006656a6365525300000000",
            "ca9d84fa0129011e1bf27d7cb71819650b59fb292b053d625c6f02b0339249b498ff7fd4b601000000025352ffffffff032173a0040000000008525253abab5152639473bb030000000009005153526a53535151d085bd0000000000086a5365ab5165655300000000",
            "e3cdbfb4014d90ae6a4401e85f7ac717adc2c035858bf6ff48979dd399d155bce1f150daea0300000002ac51a67a0d39017f6c71040000000005535200535200000000",
            "b2b6b9ab0283d9d73eeae3d847f41439cd88279c166aa805e44f8243adeb3b09e584efb1df00000000026300ffffffff7dfe653bd67ca094f8dab51007c6adaced09de2af745e175b9714ca1f5c68d050000000003ac6500aa8e596903fd3f3204000000000553ac6a6a533a2e210500000000075253acabab526392d0ee020000000008520065635200ab5200000000",
            "4314339e01de40faabcb1b970245a7f19eedbc17c507dac86cf986c2973715035cf95736ae0200000007abababababab65bde67b900151510b04000000000853ac00655200535300000000",
            "f063171b03e1830fdc1d685a30a377537363ccafdc68b42bf2e3acb908dac61ee24b37595c020000000765ac5100ab6aacf447bc8e037b89d6cadd62d960cc442d5ced901d188867b5122b42a862929ce45e7b628d010000000253aba009a1ba42b00f1490b0b857052820976c675f335491cda838fb7934d5eea0257684a2a202000000001e83cf2401a7f777030000000008ab6553526a53526a00000000",
            "cf7bdc250249e22cbe23baf6b648328d31773ea0e771b3b76a48b4748d7fbd390e88a004d30000000003ac536a4ab8cce0e097136c90b2037f231b7fde2063017facd40ed4e5896da7ad00e9c71dd70ae600000000096a0063516352525365ffffffff01b71e3e00000000000300536a00000000",
            "ac7a125a0269d35f5dbdab9948c48674616e7507413cd10e1acebeaf85b369cd8c88301b7c030000000963656aac6a530053abffffffffed94c39a582e1a46ce4c6bffda2ccdb16cda485f3a0d94b06206066da12aecfe010000000752abab63536363ef71dcfb02ee07fa0400000000016a6908c802000000000751656a6551abac688c2c2d",
            "3a1f454a03a4591e46cf1f7605a3a130b631bf4dfd81bd2443dc4fac1e0a224e74112884fe0000000005516aac6a53a87e78b55548601ffc941f91d75eab263aa79cd498c88c37fdf275a64feff89fc1710efe03000000016a39d7ef6f2a52c00378b4f8f8301853b61c54792c0f1c4e2cd18a08cb97a7668caa008d970200000002656affffffff017642b20100000000096a63535253abac6a6528271998",
            "6269e0fa0173e76e89657ca495913f1b86af5b8f1c1586bcd6c960aede9bc759718dfd5044000000000352ac530e2c7bd90219849b000000000007ab00ab6a53006319f281000000000007ab00515165ac5200000000",
            "eb2bc00604815b9ced1c604960d54beea4a3a74b5c0035d4a8b6bfec5d0c9108f143c0e99a0000000000ffffffff22645b6e8da5f11d90e5130fd0a0df8cf79829b2647957471d881c2372c527d8010000000263acffffffff1179dbaf17404109f706ae27ad7ba61e860346f63f0c81cb235d2b05d14f2c1003000000025300264cb23aaffdc4d6fa8ec0bb94eff3a2e50a83418a8e9473a16aaa4ef8b855625ed77ef40100000003ac51acf8414ad404dd328901000000000652526500006ab6261c000000000002526a72a4c9020000000006ac526500656586d2e7000000000006656aac00ac5279cd8908",
            "eba8b0de04ac276293c272d0d3636e81400b1aaa60db5f11561480592f99e6f6fa13ad387002000000070053acab536563bebb23d66fd17d98271b182019864a90e60a54f5a615e40b643a54f8408fa8512cfac927030000000963ac6a6aabac65ababffffffff890a72192bc01255058314f376bab1dc72b5fea104c154a15d6faee75dfa5dba020000000100592b3559b0085387ac7575c05b29b1f35d9a2c26a0c27903cc0f43e7e6e37d5a60d8305a030000000252abffffffff0126518f05000000000000000000",
            "185cda1a01ecf7a8a8c28466725b60431545fc7a3367ab68e34d486e8ea85ee3128e0d8384000000000465ac63abec88b7bb031c56eb04000000000965636a51005252006a7c78d5040000000007acac63abac51ac3024a40500000000086300526a51abac51464c0e8c",
            "92c9fb780138abc472e589d5b59489303f234acc838ca66ffcdf0164517a8679bb622a4267020000000153468e373d04de03fa020000000009ac006a5265ab5163006af649050000000007515153006a00658ceb59030000000001ac36afa0020000000009ab53006351ab51000000000000",
            "6f62138301436f33a00b84a26a0457ccbfc0f82403288b9cbae39986b34357cb2ff9b889b302000000045253655335a7ff6701bac9960400000000086552ab656352635200000000",
            "9981143a040a88c2484ac3abe053849e72d04862120f424f373753161997dd40505dcb4783030000000700536365536565a2e10da3f4b1c1ad049d97b33f0ae0ea48c5d7c30cc8810e144ad93be97789706a5ead180100000003636a00ffffffffbdcbac84c4bcc87f03d0ad83fbe13b369d7e42ddb3aecf40870a37e814ad8bb5010000000963536a5100636a53abffffffff883609905a80e34202101544f69b58a0b4576fb7391e12a769f890eef90ffb72020000000651656352526affffffff04243660000000000004ab5352534a9ce001000000000863656363ab6a53652df19d030000000003ac65acedc51700000000000000000000",
            "49f7d0b6037bba276e910ad3cd74966c7b3bc197ffbcfefd6108d6587006947e97789835ea0300000008526a52006a650053ffffffff8d7b6c07cd10f4c4010eac7946f61aff7fb5f3920bdf3467e939e58a1d4100ab03000000076aac63ac535351ffffffff8f48c3ba2d52ad67fbcdc90d8778f3c8a3894e3c35b9730562d7176b81af23c80100000003ab5265ffffffff0301e3ef0300000000046a525353e899ac0500000000075153ab6a65abac259bea0400000000007b739972",
            "58a4fed801fbd8d92db9dfcb2e26b6ff10b120204243fee954d7dcb3b4b9b53380e7bb8fb60100000003006351ffffffff02a0795b050000000006536351ac6aac2718d00200000000075151acabac515354d21ba1",
            "17fad0d303da0d764fedf9f2887a91ea625331b28704940f41e39adf3903d8e75683ef6d46020000000151ffffffffff376eea4e880bcf0f03d33999104aafed2b3daf4907950bb06496af6b51720a020000000900636a63525253525196521684f3b08497bad2c660b00b43a6a517edc58217876eb5e478aa3b5fda0f29ee1bea00000000046aacab6affffffff03dde8e2050000000007ac5365ac51516a14772e000000000005630000abacbbb360010000000006ab5251ab656a50f180f0",
            "5a2257df03554550b774e677f348939b37f8e765a212e566ce6b60b4ea8fed4c9504b7f7d1000000000653655265ab5258b67bb931df15b041177cf9599b0604160b79e30f3d7a594e7826bae2c29700f6d8f8f40300000005515300ac6a159cf8808a41f504eb5c2e0e8a9279f3801a5b5d7bc6a70515fbf1c5edc875bb4c9ffac500000000050063510052ffffffff0422a90105000000000965006a650000516a006417d2020000000006526363ab00524d969d0100000000035153acc4f077040000000005ac5200636500000000",
            "e0032ad601269154b3fa72d3888a3151da0aed32fb2e1a15b3ae7bee57c3ddcffff76a1321010000000100110d93ae03f5bd080100000000075263516a6551002871e60100000000046a005252eaa753040000000004ab6aab526e325c71",
            "014b2a5304d46764817aca180dca50f5ab25f2e0d5749f21bb74a2f8bf6b8b7b3fa8189cb7030000000965ac5165ab6a51ac6360ecd91e8abc7e700a4c36c1a708a494c94bb20cbe695c408543146566ab22be43beae9103000000045163ab00ffffffffffa48066012829629a9ec06ccd4905a05df0e2b745b966f6a269c9c8e13451fc00000000026565ffffffffc40ccadc21e65fe8a4b1e072f4994738ccaf4881ae6fede2a2844d7da4d199ab02000000065152ab536aabffffffff01b6e054030000000004515352ab3e063432",
            "1201ab5d04f89f07c0077abd009762e59db4bb0d86048383ba9e1dad2c9c2ad96ef660e6d00200000007ab6a65ac5200652466fa5143ab13d55886b6cdc3d0f226f47ec1c3020c1c6e32602cd3428aceab544ef43e00000000086a6a6a526a6a5263ffffffffd5be0b0be13ab75001243749c839d779716f46687e2e9978bd6c9e2fe457ee48020000000365abab1e1bac0f72005cf638f71a3df2e3bbc0fa35bf00f32d9c7dc9c39a5e8909f7d53170c8ae0200000008ab6a51516363516affffffff02f0a6210500000000036300ac867356010000000009acab65ac6353536a659356d367",
            "344fa11e01c19c4dd232c77742f0dd0aeb3695f18f76da627628741d0ee362b0ea1fb3a2180200000007635151005100529bab25af01937c1f0500000000055153ab53656e7630af",
            "b2fda1950191358a2b855f5626a0ebc830ab625bea7480f09f9cd3b388102e35c0f303124c030000000565ac65ab53ffffffff03f9c5ec04000000000765ab51516551650e2b9f0500000000045365525284e8f6040000000001ac00000000",
            "a4a6bbd201aa5d882957ac94f2c74d4747ae32d69fdc765add4acc2b68abd1bdb8ee333d6e0300000008516a6552515152abffffffff02c353cb040000000007ac6351ab51536588bd320500000000066552525253ac00000000",
            "83a583d204d926f2ee587a83dd526cf1e25a44bb668e45370798f91a2907d184f7cddcbbc7030000000700ab6565536a539f71d3776300dffdfa0cdd1c3784c9a1f773e34041ca400193612341a9c42df64e3f550e01000000050052515251ffffffff52dab2034ab0648553a1bb8fc4e924b2c89ed97c18dfc8a63e248b454035564b01000000015139ab54708c7d4d2c2886290f08a5221cf69592a810fd1979d7b63d35c271961e710424fd0300000005ac65ac5251ffffffff01168f7c030000000000a85e5fb0",
            "ffd35d51042f290108fcb6ea49a560ba0a6560f9181da7453a55dfdbdfe672dc800b39e7320200000006630065516a65f2166db2e3827f44457e86dddfd27a8af3a19074e216348daa0204717d61825f198ec0030100000006ab51abab00abffffffffdf41807adb7dff7db9f14d95fd6dc4e65f8402c002d009a3f1ddedf6f4895fc8030000000500ab006a65a5a848345052f860620abd5fcd074195548ce3bd0839fa9ad8642ed80627bf43a0d47dbd010000000765ab006a656a53b38cdd6502a186da05000000000765ab00ab006a53527c0e0100000000085365ab51acacac52534bd1b1",
            "6c9a4b98013c8f1cae1b1df9f0f2de518d0c50206a0ab871603ac682155504c0e0ce946f460100000000ffffffff04e9266305000000000753535100ac6aacded39e04000000000365ac6ab93ccd010000000002515397bf3d050000000003ab636300000000",
            "c4b80f850323022205b3e1582f1ed097911a81be593471a8dce93d5c3a7bded92ef6c7c1260100000002006affffffff70294d62f37c3da7c5eae5d67dce6e1b28fedd7316d03f4f48e1829f78a88ae801000000096a5200530000516351f6b7b544f7c39189d3a2106ca58ce4130605328ce7795204be592a90acd81bef517d6f170200000000ffffffff012ab8080000000000075100006365006335454c1e",
            "8ab69ed50351b47b6e04ac05e12320984a63801716739ed7a940b3429c9c9fed44d3398ad40300000006536a516a52638171ef3a46a2adb8025a4884b453889bc457d63499971307a7e834b0e76eec69c943038a0300000000ffffffff566bb96f94904ed8d43d9d44a4a6301073cef2c011bf5a12a89bedbaa03e4724030000000265acb606affd01edea38050000000008515252516aacac6300000000",
            "2484991e047f1cf3cfe38eab071f915fe86ebd45d111463b315217bf9481daf0e0d10902a402000000006e71a424eb1347ffa638363604c0d5eccbc90447ff371e000bf52fc743ec832851bb564a0100000001abffffffffef7d014fad3ae7927948edbbb3afe247c1bcbe7c4c8f5d6cf97c799696412612020000000851536a5353006a001dfee0d7a0dd46ada63b925709e141863f7338f34f7aebde85d39268ae21b77c3068c01d0000000008535151ab00636563ffffffff018478070200000000095200635365ac52ab5341b08cd3",
            "54839ef9026f65db30fc9cfcb71f5f84d7bb3c48731ab9d63351a1b3c7bc1e7da22bbd508e0300000000442ad138f170e446d427d1f64040016032f36d8325c3b2f7a4078766bdd8fb106e52e8d20000000003656500ffffffff02219aa101000000000851ababac52ab00659646bd02000000000552acacabac24c394a5",
            "5036d7080434eb4eef93efda86b9131b0b4c6a0c421e1e5feb099a28ff9dd8477728639f77030000000951516aab535152ab5391429be9cce85d9f3d358c5605cf8c3666f034af42740e94d495e28b9aaa1001ba0c87580300000008006552ab00ab006affffffffd838978e10c0c78f1cd0a0830d6815f38cdcc631408649c32a25170099669daa0000000002acab8984227e804ad268b5b367285edcdf102d382d027789250a2c0641892b480c21bf84e3fb0100000000b518041e023d8653010000000001004040fb0100000000080051ac5200636a6300000000",
            "9ad5ccf503fa4facf6a27b538bc910cce83c118d6dfd82f3fb1b8ae364a1aff4dcefabd38f03000000096365655263ac655300807c48130c5937190a996105a69a8eba585e0bd32fadfc57d24029cbed6446d30ebc1f100100000004000053650f0ccfca1356768df7f9210cbf078a53c72e0712736d9a7a238e0115faac0ca383f219d0010000000600ab536552002799982b0221b8280000000000000c41320000000000086552ac6365636a6595f233a3",
            "669538a204047214ce058aed6a07ca5ad4866c821c41ac1642c7d63ed0054f84677077a84f030000000853abacab6a655353ffffffff70c2a071c115282924e3cb678b13800c1d29b6a028b3c989a598c491bc7c76c5030000000752ac52ac5163ac80420e8a6e43d39af0163271580df6b936237f15de998e9589ec39fe717553d415ac02a4030000000463635153184ad8a5a4e69a8969f71288c331aff3c2b7d1b677d2ebafad47234840454b624bf7ac1d03000000056a63abab63df38c24a02fbc63a040000000002ab535ec3dc050000000002536500000000",
            "972128b904e7b673517e96e98d80c0c8ceceae76e2f5c126d63da77ffd7893fb53308bb2da0300000006ac6552ab52acffffffff4cac767c797d297c079a93d06dc8569f016b4bf7a7d79b605c526e1d36a40e2202000000095365ab636aac6a6a6a69928d2eddc836133a690cfb72ec2d3115bf50fb3b0d10708fa5d2ebb09b4810c426a1db01000000060052526300001e8e89585da7e77b2dd2e30625887f0660accdf29e53a614d23cf698e6fc8ab03310e87700000000076a520051acac6555231ddb0330ec2d03000000000200abfaf457040000000004ab6a6352bdc42400000000000153d6dd2f04",
            "5374f0c603d727f63006078bd6c3dce48bd5d0a4b6ea00a47e5832292d86af258ea0825c260000000009655353636352526a6af2221067297d42a9f8933dfe07f61a574048ff9d3a44a3535cd8eb7de79fb7c45b6f47320200000003ac006affffffff153d917c447d367e75693c5591e0abf4c94bbdd88a98ab8ad7f75bfe69a08c470200000005ac65516365ffffffff037b5b7b000000000001515dc4d904000000000004bb26010000000004536a6aac00000000",
            "c441132102cc82101b6f31c1025066ab089f28108c95f18fa67db179610247086350c163bd010000000651525263ab00ffffffff9b8d56b1f16746f075249b215bdb3516cbbe190fef6292c75b1ad8a8988897c3000000000751ab6553abab00ffffffff02f9078b000000000009ab0053ac51ac00ab51c0422105000000000651006563525200000000",
            "8bff9d170419fa6d556c65fa227a185fe066efc1decf8a1c490bc5cbb9f742d68da2ab7f320100000007ab000053525365a7a43a80ab9593b9e8b6130a7849603b14b5c9397a190008d89d362250c3a2257504eb810200000007acabacac00ab51ee141be418f003e75b127fd3883dbf4e8c3f6cd05ca4afcaac52edd25dd3027ae70a62a00000000008ac52526a5200536affffffffb8058f4e1d7f220a1d1fa17e96d81dfb9a304a2de4e004250c9a576963a586ae0300000005abacac5363b9bc856c039c01d804000000000951656aac53005365acb0724e00000000000565abab63acea7c7a0000000000036a00ac00000000",
            "0e1633b4041c50f656e882a53fde964e7f0c853b0ada0964fc89ae124a2b7ffc5bc97ea6230100000006ac6aacacabacffffffff2e35f4dfcad2d53ea1c8ada8041d13ea6c65880860d96a14835b025f76b1fbd9000000000351515121270867ef6bf63a91adbaf790a43465c61a096acc5a776b8e5215d4e5cd1492e611f761000000000600ac6aab5265ffffffff63b5fc39bcac83ca80ac36124abafc5caee608f9f63a12479b68473bd4bae769000000000965ac52acac5263acabffffffff0163153e020000000008ab005165ab65515300000000",
            "2b052c24022369e956a8d318e38780ef73b487ba6a8f674a56bdb80a9a63634c6110fb5154010000000251acffffffff48fe138fb7fdaa014d67044bc05940f4127e70c113c6744fbd13f8d51d45143e01000000005710db3804e01aa9030000000008acac6a516a5152abfd55aa01000000000751ab510000ac636d6026010000000000b97da9000000000000fddf3b53",
            "7888b71403f6d522e414d4ca2e12786247acf3e78f1918f6d727d081a79813d129ee8befce0100000009ab516a6353ab6365abffffffff4a882791bf6400fda7a8209fb2c83c6eef51831bdf0f5dacde648859090797ec030000000153ffffffffbb08957d59fa15303b681bad19ccf670d7d913697a2f4f51584bf85fcf91f1f30200000008526565ac52ac63acffffffff0227c0e8050000000001ac361dc801000000000800515165ab00ab0000000000",
            "cc4dda57047bd0ca6806243a6a4b108f7ced43d8042a1acaa28083c9160911cf47eab910c40200000007526a0000ab6a63e4154e581fcf52567836c9a455e8b41b162a78c85906ccc1c2b2b300b4c69caaaa2ba0230300000008ab5152ac5100ab65ffffffff69696b523ed4bd41ecd4d65b4af73c9cf77edf0e066138712a8e60a04614ea1c0300000004ab6a000016c9045c7df7836e05ac4b2e397e2dd72a5708f4a8bf6d2bc36adc5af3cacefcf074b8b403000000065352ac5252acffffffff01d7e380050000000000cf4e699a",
            "b7877f82019c832707a60cf14fba44cfa254d787501fdd676bd58c744f6e951dbba0b3b77f0200000009ac515263ac53525300a5a36e500148f89c0500000000085265ac6a6a65acab00000000",
            "aeb14046045a28cc59f244c2347134d3434faaf980961019a084f7547218785a2bd03916f3000000000165f852e6104304955bda5fa0b75826ee176211acc4a78209816bbb4419feff984377b2352200000000003a94a5032df1e0d60390715b4b188c330e4bb7b995f07cdef11ced9d17ee0f60bb7ffc8e0100000002516513e343a5c1dc1c80cd4561e9dddad22391a2dbf9c8d2b6048e519343ca1925a9c6f0800a020000000665516365ac513180144a0290db27000000000006ab655151ab5138b187010000000007ab5363abac516a9e5cd98a",
            "57a5a04c0278c8c8e243d2df4bb716f81d41ac41e2df153e7096f5682380c4f441888d9d260300000004ab63ab6afdbe4203525dff42a7b1e628fe22bccaa5edbb34d8ab02faff198e085580ea5fcdb0c61b0000000002ac6affffffff03375e6c05000000000663ab516a6a513cb6260400000000007ca328020000000006516a636a52ab94701cc7",
            "7e27c42d0279c1a05eeb9b9faedcc9be0cab6303bde351a19e5cbb26dd0d594b9d74f40d2b020000000200518c8689a08a01e862d5c4dcb294a2331912ff11c13785be7dce3092f154a005624970f84e0200000000500cf5a601e74c1f0000000000076aab52636a6a5200000000",
            "2b3470dd028083910117f86614cdcfb459ee56d876572510be4df24c72e8f58c70d5f5948b03000000066aab65635265da2c3aac9d42c9baafd4b655c2f3efc181784d8cba5418e053482132ee798408ba43ccf90300000000ffffffff047dda4703000000000765516a52ac53009384a603000000000651636a63ab6a8cf57a03000000000352ab6a8cf6a405000000000952636a6a6565525100661e09cb",
            "3a5644a9010f199f253f858d65782d3caec0ac64c3262b56893022b9796086275c9d4d097b02000000009d168f7603a67b30050000000007ac51536a0053acd9d88a050000000007655363535263ab3cf1f403000000000352ac6a00000000",
            "bda1ff6804a3c228b7a12799a4c20917301dd501c67847d35da497533a606701ad31bf9d5e0300000001ac16a6c5d03cf516cd7364e4cbbf5aeccd62f8fd03cb6675883a0636a7daeb650423cb1291010000000500656553ac4a63c30b6a835606909c9efbae1b2597e9db020c5ecfc0642da6dc583fba4e84167539a8020000000865525353515200acffffffff990807720a5803c305b7da08a9f24b92abe343c42ac9e917a84e1f335aad785d00000000026a52ffffffff04981f20030000000001ab8c762200000000000253ab690b9605000000000151ce88b301000000000753526a6a51006500000000",
            "db4904e6026b6dd8d898f278c6428a176410d1ffbde75a4fa37cda12263108ccd4ca6137440100000007656a0000515263ffffffff1db7d5005c1c40da0ed17b74cf6b2a6ee2c33c9e0bacda76c0da2017dcac2fc70200000004abab6a53ffffffff0454cf2103000000000153463aef000000000009ab6a630065ab52636387e0ed050000000000e8d16f05000000000352ac63e4521b22",
            "dca31ad10461ead74751e83d9a81dcee08db778d3d79ad9a6d079cfdb93919ac1b0b61871102000000086500525365ab51ac7f7e9aed78e1ef8d213d40a1c50145403d196019985c837ffe83836222fe3e5955e177e70100000006525152525300ffffffff5e98482883cc08a6fe946f674cca479822f0576a43bf4113de9cbf414ca628060100000006ac53516a5253ffffffff07490b0b898198ec16c23b75d606e14fa16aa3107ef9818594f72d5776805ec502000000036a0052ffffffff01932a2803000000000865ab6551ac6a516a2687aa06",
            "e14e1a9f0442ab44dfc5f6d945ad1ff8a376bc966aad5515421e96ddbe49e529614995cafc03000000055165515165fffffffff97582b8290e5a5cfeb2b0f018882dbe1b43f60b7f45e4dd21dbd3a8b0cfca3b0200000000daa267726fe075db282d694b9fee7d6216d17a8c1f00b2229085495c5dc5b260c8f8cd5d000000000363ac6affffffffaab083d22d0465471c896a438c6ac3abf4d383ae79420617a8e0ba8b9baa872b010000000963526563ac5363ababd948b5ce022113440200000000076a636552006a53229017040000000000e6f62ac8",
            "69df842a04c1410bfca10896467ce664cfa31c681a5dac10106b34d4b9d4d6d0dc1eac01c1000000000551536a5165269835ca4ad7268667b16d0a2df154ec81e304290d5ed69e0069b43f8c89e673328005e200000000076a5153006aacabffffffffc9314bd80b176488f3d634360fcba90c3a659e74a52e100ac91d3897072e3509010000000765abac51636363ffffffff0e0768b13f10f0fbd2fa3f68e4b4841809b3b5ba0e53987c3aaffcf09eee12bf0300000008ac535263526a53ac514f4c2402da8fab0400000000001ef15201000000000451526a52d0ec9aca",
            "e4fec9f10378a95199c1dd23c6228732c9de0d7997bf1c83918a5cfd36012476c0c3cba24002000000085165536500ac0000ad08ab93fb49d77d12a7ccdbb596bc5110876451b53a79fdce43104ff1c316ad63501de801000000046a6352ab76af9908463444aeecd32516a04dd5803e02680ed7f16307242a794024d93287595250f4000000000089807279041a82e603000000000200521429100200000000055253636a63f20b940400000000004049ed04000000000500ab5265ab43dfaf7d",
            "4000d3600100b7a3ff5b41ec8d6ccdc8b2775ad034765bad505192f05d1f55d2bc39d0cbe10100000007ab5165ac6a5163ffffffff034949150100000000026a6a92c9f6000000000008ab6553ab6aab635200e697040000000007636a5353525365237ae7d2",
            "eabc0aa701fe489c0e4e6222d72b52f083166b49d63ad1410fb98caed027b6a71c02ab830c03000000075253ab63530065ffffffff01a5dc0b05000000000253533e820177",
            "d48d55d304aad0139783b44789a771539d052db565379f668def5084daba0dfd348f7dcf6b00000000006826f59e5ffba0dd0ccbac89c1e2d69a346531d7f995dea2ca6d7e6d9225d81aec257c6003000000096a655200ac656552acffffffffa188ffbd5365cae844c8e0dea6213c4d1b2407274ae287b769ab0bf293e049eb0300000005ac6a6aab51ad1c407c5b116ca8f65ed496b476183f85f072c5f8a0193a4273e2015b1cc288bf03e9e2030000000252abffffffff04076f44040000000006655353abab53be6500050000000003ac65ac3c15040500000000095100ab536353516a52ed3aba04000000000900ac53ab53636aabac00000000",
            "9746f45b039bfe723258fdb6be77eb85917af808211eb9d43b15475ee0b01253d33fc3bfc502000000065163006a655312b12562dc9c54e11299210266428632a7d0ee31d04dfc7375dcad2da6e9c11947ced0e000000000009074095a5ac4df057554566dd04740c61490e1d3826000ad9d8f777a93373c8dddc4918a00000000025351ffffffff01287564030000000004636a00ab00000000",
            "df0a32ae01c4672fd1abd0b2623aae0a1a8256028df57e532f9a472d1a9ceb194267b6ee190200000009536a6a51516a525251b545f9e803469a2302000000000465526500810631040000000000441f5b050000000006530051006aaceb183c76",
            "b1c0b71804dff30812b92eefb533ac77c4b9fdb9ab2f77120a76128d7da43ad70c20bbfb990200000002536392693e6001bc59411aebf15a3dc62a6566ec71a302141b0c730a3ecc8de5d76538b30f55010000000665535252ac514b740c6271fb9fe69fdf82bf98b459a7faa8a3b62f3af34943ad55df4881e0d93d3ce0ac0200000000c4158866eb9fb73da252102d1e64a3ce611b52e873533be43e6883137d0aaa0f63966f060000000001abffffffff04a605b604000000000851006a656a630052f49a0300000000000252515a94e1050000000009abac65ab0052abab00fd8dd002000000000651535163526a2566852d",
            "3657e4260304ccdc19936e47bdf058d36167ee3d4eb145c52b224eff04c9eb5d1b4e434dfc0000000001ab58aefe57707c66328d3cceef2e6f56ab6b7465e587410c5f73555a513ace2b232793a74400000000036a006522e69d3a785b61ad41a635d59b3a06b2780a92173f85f8ed428491d0aaa436619baa9c4501000000046351abab2609629902eb7793050000000000a1b967040000000003525353a34d6192",
            "a0eb6dc402994e493c787b45d1f946d267b09c596c5edde043e620ce3d59e95b2b5b93d43002000000096a5252526aac63ab6555694287a279e29ee491c177a801cd685b8744a2eab83824255a3bcd08fc0e3ea13fb8820000000009abab6365ab52ab0063ffffffff029e424a040000000008acab53ab516a636a23830f0400000000016adf49c1f9",
            "6e67c0d3027701ef71082204c85ed63c700ef1400c65efb62ce3580d187fb348376a23e9710200000001655b91369d3155ba916a0bc6fe4f5d94cad461d899bb8aaac3699a755838bfc229d6828920010000000765536353526a52ffffffff04c0c792000000000005650052535372f79e000000000001527fc0ee010000000005ac5300ab65d1b3e902000000000251aba942b278",
            "8f53639901f1d643e01fc631f632b7a16e831d846a0184cdcda289b8fa7767f0c292eb221a00000000046a53abacffffffff037a2daa01000000000553ac6a6a51eac349020000000005ac526552638421b3040000000007006a005100ac63048a1492",
            "b3cad3a7041c2c17d90a2cd994f6c37307753fa3635e9ef05ab8b1ff121ca11239a0902e700300000009ab635300006aac5163ffffffffcec91722c7468156dce4664f3c783afef147f0e6f80739c83b5f09d5a09a57040200000004516a6552ffffffff969d1c6daf8ef53a70b7cdf1b4102fb3240055a8eaeaed2489617cd84cfd56cf020000000352ab53ffffffff46598b6579494a77b593681c33422a99559b9993d77ca2fa97833508b0c169f80200000009655300655365516351ffffffff04d7ddf800000000000853536a65ac6351ab09f3420300000000056aab65abac33589d04000000000952656a65655151acac944d6f0400000000006a8004ba",
            "cf781855040a755f5ba85eef93837236b34a5d3daeb2dbbdcf58bb811828d806ed05754ab8010000000351ac53ffffffffda1e264727cf55c67f06ebcc56dfe7fa12ac2a994fecd0180ce09ee15c480f7d00000000096351516a51acac00ab53dd49ff9f334befd6d6f87f1a832cddfd826a90b78fd8cf19a52cb8287788af94e939d6020000000700525251ac526310d54a7e8900ed633f0f6f0841145aae7ee0cbbb1e2a0cae724ee4558dbabfdc58ba6855010000000552536a53abfd1b101102c51f910500000000096300656a525252656a300bee010000000009ac52005263635151abe19235c9",
            ]
    }
}
