// LNP/BP universal invoice library implementing LNPBP-38 standard
// Written in 2021 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the MIT License
// along with this software.
// If not, see <https://opensource.org/licenses/MIT>.

use chrono::NaiveDateTime;
#[cfg(feature = "serde")]
use serde_with::{As, DisplayFromStr};
use std::cmp::Ordering;
use std::fmt::{self, Display, Formatter, Write};
use std::io;
use std::str::FromStr;

use bitcoin::hashes::sha256d;
use bitcoin::secp256k1::{self, Signature};
use bitcoin::Address;
use internet2::tlv;
use lnp::features::InitFeatures;
use lnp::payment::ShortChannelId;
use lnpbp::bech32::{self, Blob, FromBech32Str, ToBech32String};
use lnpbp::chain::{AssetId, Chain};
use lnpbp::seals::OutpointHash;
use miniscript::{descriptor::DescriptorPublicKey, Descriptor};
use strict_encoding::{StrictDecode, StrictEncode};
use wallet::{HashLock, Psbt};

// TODO: Derive `Eq` & `Hash` once Psbt will support them
#[cfg_attr(
    feature = "serde",
    serde_as,
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[derive(
    Getters,
    Clone,
    PartialEq,
    Debug,
    Display,
    StrictEncode,
    StrictDecode,
    LightningEncode,
    LightningDecode,
)]
#[display(Invoice::to_bech32_string)]
pub struct Invoice {
    /// Version byte, always 0 for the initial version
    version: u8,

    /// Amount in the specified asset - a price per single item, if `quantity`
    /// options is set
    #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
    amount: AmountExt,

    /// Main beneficiary. Separating the first beneficiary into a standalone
    /// field allows to ensure that there is always at lease one beneficiary
    /// at compile time
    beneficiary: Beneficiary,

    /// List of beneficiary ordered in most desirable-first order, which follow
    /// `beneficiary` value
    #[tlv(type = 1)]
    alt_beneficiaries: Vec<Beneficiary>,

    /// AssetId can also be used to define blockchain. If it's empty it implies
    /// bitcoin mainnet
    #[tlv(type = 2)]
    #[cfg_attr(
        feature = "serde",
        serde(with = "As::<Option<DisplayFromStr>>")
    )]
    asset: Option<AssetId>,

    /// Interval between recurrent payments
    #[tlv(type = 3)]
    recurrent: Recurrent,

    #[tlv(type = 4)]
    #[cfg_attr(
        feature = "serde",
        serde(with = "As::<Option<DisplayFromStr>>")
    )]
    expiry: Option<NaiveDateTime>, // Must be mapped to i64

    #[tlv(type = 5)]
    quantity: Option<Quantity>,

    /// If the price of the asset provided by fiat provider URL goes below this
    /// limit the merchant will not accept the payment and it will become
    /// expired
    #[tlv(type = 6)]
    currency_requirement: Option<CurrencyData>,

    #[tlv(type = 7)]
    merchant: Option<String>,

    #[tlv(type = 8)]
    purpose: Option<String>,

    #[tlv(type = 9)]
    details: Option<Details>,

    #[tlv(type = 0)]
    #[cfg_attr(
        feature = "serde",
        serde(with = "As::<Option<DisplayFromStr>>")
    )]
    signature: Option<Signature>,

    #[tlv(unknown)]
    #[cfg_attr(feature = "serde", serde(skip))]
    unknown: tlv::Map,
    // TODO: Add RGB feature vec optional field
}

impl bech32::Strategy for Invoice {
    const HRP: &'static str = "i";

    type Strategy = bech32::strategies::CompressedStrictEncoding;
}

impl FromStr for Invoice {
    type Err = bech32::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Invoice::from_bech32_str(s)
    }
}

impl Ord for Invoice {
    fn cmp(&self, other: &Self) -> Ordering {
        self.to_string().cmp(&other.to_string())
    }
}

impl PartialOrd for Invoice {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::hash::Hash for Invoice {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.to_string().hash(state)
    }
}

impl Eq for Invoice {}

impl Invoice {
    pub fn new(
        beneficiary: Beneficiary,
        amount: Option<u64>,
        asset: Option<AssetId>,
    ) -> Invoice {
        Invoice {
            version: 0,
            amount: amount
                .map(|value| AmountExt::Normal(value))
                .unwrap_or(AmountExt::Any),
            beneficiary,
            alt_beneficiaries: vec![],
            asset,
            recurrent: Default::default(),
            expiry: None,
            quantity: None,
            currency_requirement: None,
            merchant: None,
            purpose: None,
            details: None,
            signature: None,
            unknown: Default::default(),
        }
    }

    pub fn native(
        beneficiary: Beneficiary,
        amount: Option<u64>,
        chain: &Chain,
    ) -> Invoice {
        Invoice::new(
            beneficiary,
            amount,
            if chain == &Chain::Mainnet {
                None
            } else {
                Some(AssetId::native(&chain))
            },
        )
    }

    pub fn beneficiaries(&self) -> BeneficiariesIter {
        BeneficiariesIter {
            invoice: self,
            index: 0,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BeneficiariesIter<'a> {
    invoice: &'a Invoice,
    index: usize,
}

impl<'a> Iterator for BeneficiariesIter<'a> {
    type Item = &'a Beneficiary;

    fn next(&mut self) -> Option<Self::Item> {
        self.index += 1;
        if self.index == 1 {
            Some(&self.invoice.beneficiary)
        } else {
            self.invoice.alt_beneficiaries.get(self.index - 2)
        }
    }
}

#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Debug,
    Display,
    From,
    StrictEncode,
    StrictDecode,
)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", rename = "lowercase")
)]
#[non_exhaustive]
pub enum Recurrent {
    #[display("non-recurrent")]
    NonRecurrent,

    #[display("each {0} seconds")]
    Seconds(u64),

    #[display("each {0} months")]
    Months(u8),

    #[display("each {0} years")]
    Years(u8),
}

impl lightning_encoding::Strategy for Recurrent {
    type Strategy = lightning_encoding::strategies::AsStrict;
}

impl Default for Recurrent {
    fn default() -> Self {
        Recurrent::NonRecurrent
    }
}

// TODO: Derive `Eq` & `Hash` once Psbt will support them
#[cfg_attr(
    feature = "serde",
    serde_as,
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", rename = "lowercase", tag = "format")
)]
#[derive(
    Clone, PartialEq, Debug, Display, From, StrictEncode, StrictDecode,
)]
#[display(inner)]
#[non_exhaustive]
pub enum Beneficiary {
    /// Addresses are useful when you do not like to leak public key
    /// information
    #[from]
    Address(
        #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
        Address,
    ),

    /// Used by protocols that work with existing UTXOs and can assign some
    /// client-validated data to them (like in RGB). We always hide the real
    /// UTXO behind the hashed version (using some salt)
    #[from]
    BlindUtxo(
        #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
        OutpointHash,
    ),

    /// Miniscript-based descriptors allowing custom derivation & key
    /// generation
    #[from]
    Descriptor(
        #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
        Descriptor<DescriptorPublicKey>,
    ),

    /// Full transaction template in PSBT format
    #[from]
    // TODO: Fix display once PSBT implement `Display`
    #[display("PSBT!")]
    Psbt(Psbt),

    /// Lightning node receiving the payment. Not the same as lightning invoice
    /// since many of the invoice data now will be part of [`Invoice`] here.
    #[from]
    Lightning(LnAddress),

    /// Fallback option for all future variants
    Unknown(
        #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
        Blob,
    ),
}

impl lightning_encoding::Strategy for Beneficiary {
    type Strategy = lightning_encoding::strategies::AsStrict;
}

#[derive(
    Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Display, Error,
)]
#[display(doc_comments)]
/// Incorrect beneficiary format
pub struct BeneficiaryParseError;

// TODO: Since we can't present full beneficiary data in a string form (because
//       of the lightning part) we have to remove this implementation once
//       serde_with will be working
impl FromStr for Beneficiary {
    type Err = BeneficiaryParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(address) = Address::from_str(s) {
            Ok(Beneficiary::Address(address))
        } else if let Ok(outpoint) = OutpointHash::from_str(s) {
            Ok(Beneficiary::BlindUtxo(outpoint))
        } else if let Ok(descriptor) =
            Descriptor::<DescriptorPublicKey>::from_str(s)
        {
            Ok(Beneficiary::Descriptor(descriptor))
        } else {
            Err(BeneficiaryParseError)
        }
    }
}

#[cfg_attr(
    feature = "serde",
    serde_as,
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[derive(
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Debug,
    Display,
    StrictEncode,
    StrictDecode,
    LightningEncode,
    LightningDecode,
)]
#[display("{node_id}")]
pub struct LnAddress {
    #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
    pub node_id: secp256k1::PublicKey,
    pub features: InitFeatures,
    #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
    pub lock: HashLock, /* When PTLC will be available the same field will
                         * be re-used for them + the
                         * use will be indicated with a
                         * feature flag */
    pub min_final_cltv_expiry: Option<u16>,
    pub path_hints: Vec<LnPathHint>,
}

/// Path hints for a lightning network payment, equal to the value of the `r`
/// key of the lightning BOLT-11 invoice
/// <https://github.com/lightningnetwork/lightning-rfc/blob/master/11-payment-encoding.md#tagged-fields>
#[cfg_attr(
    feature = "serde",
    serde_as,
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[derive(
    Copy,
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Debug,
    Display,
    StrictEncode,
    StrictDecode,
    LightningEncode,
    LightningDecode,
)]
#[display("{short_channel_id}@{node_id}")]
pub struct LnPathHint {
    #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
    pub node_id: secp256k1::PublicKey,
    #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
    pub short_channel_id: ShortChannelId,
    pub fee_base_msat: u32,
    pub fee_proportional_millionths: u32,
    pub cltv_expiry_delta: u16,
}

#[derive(
    Copy,
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Debug,
    Display,
    From,
    StrictEncode,
    StrictDecode,
)]
pub enum AmountExt {
    /// Payments for any amount is accepted: useful for charity/donations, etc
    #[display("any")]
    Any,

    #[from]
    #[display(inner)]
    Normal(u64),

    #[display("{0}.{1}")]
    Milli(u64, u16),
}

impl Default for AmountExt {
    fn default() -> Self {
        AmountExt::Any
    }
}

impl lightning_encoding::Strategy for AmountExt {
    type Strategy = lightning_encoding::strategies::AsStrict;
}

#[derive(
    Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Display, Error, From,
)]
#[display(doc_comments)]
#[from(std::num::ParseIntError)]
/// Incorrect beneficiary format
pub struct AmountParseError;

impl FromStr for AmountExt {
    type Err = AmountParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().to_lowercase() == "any" {
            return Ok(AmountExt::Any);
        }
        let mut split = s.split(".");
        Ok(match (split.next(), split.next()) {
            (Some(amt), None) => AmountExt::Normal(amt.parse()?),
            (Some(int), Some(frac)) => {
                AmountExt::Milli(int.parse()?, frac.parse()?)
            }
            _ => Err(AmountParseError)?,
        })
    }
}

#[derive(
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Debug,
    Display,
    StrictEncode,
    StrictDecode,
    LightningEncode,
    LightningDecode,
)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("{source}")]
pub struct Details {
    #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
    pub commitment: sha256d::Hash,
    pub source: String, // Url
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, From)]
// TODO: Move to amplify library
pub struct Iso4217([u8; 3]);

impl Display for Iso4217 {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_char(self.0[0].into())?;
        f.write_char(self.0[1].into())?;
        f.write_char(self.0[2].into())
    }
}

#[derive(
    Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display, Error,
)]
#[display(doc_comments)]
pub enum Iso4217Error {
    /// Wrong string length to parse ISO4217 data
    WrongLen,
}

impl FromStr for Iso4217 {
    type Err = Iso4217Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.bytes().len() != 3 {
            return Err(Iso4217Error::WrongLen);
        }

        let mut inner = [0u8; 3];
        inner.copy_from_slice(&s.bytes().collect::<Vec<u8>>()[0..3]);
        Ok(Iso4217(inner))
    }
}

impl StrictEncode for Iso4217 {
    fn strict_encode<E: io::Write>(
        &self,
        mut e: E,
    ) -> Result<usize, strict_encoding::Error> {
        e.write(&self.0)?;
        Ok(3)
    }
}

impl StrictDecode for Iso4217 {
    fn strict_decode<D: io::Read>(
        mut d: D,
    ) -> Result<Self, strict_encoding::Error> {
        let mut me = Self([0u8; 3]);
        d.read_exact(&mut me.0)?;
        Ok(me)
    }
}

impl lightning_encoding::Strategy for Iso4217 {
    type Strategy = lightning_encoding::strategies::AsStrict;
}

#[cfg_attr(
    feature = "serde",
    serde_as,
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[derive(
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Debug,
    Display,
    StrictEncode,
    StrictDecode,
    LightningEncode,
    LightningDecode,
)]
#[display("{coins}.{fractions} {iso4217}")]
pub struct CurrencyData {
    #[cfg_attr(feature = "serde", serde(with = "As::<DisplayFromStr>"))]
    pub iso4217: Iso4217,
    pub coins: u32,
    pub fractions: u8,
    pub price_provider: String, // Url,
}

#[derive(
    Copy,
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Debug,
    From,
    StrictEncode,
    StrictDecode,
    LightningEncode,
    LightningDecode,
)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub struct Quantity {
    pub min: u32, // We will default to zero
    pub max: Option<u32>,
    #[from]
    pub default: u32,
}

impl Default for Quantity {
    fn default() -> Self {
        Self {
            min: 0,
            max: None,
            default: 1,
        }
    }
}

impl Display for Quantity {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} items", self.default)?;
        match (self.min, self.max) {
            (0, Some(max)) => write!(f, " (or any amount up to {})", max),
            (0, None) => Ok(()),
            (_, Some(max)) => write!(f, " (or from {} to {})", self.min, max),
            (_, None) => write!(f, " (or any amount above {})", self.min),
        }
    }
}
