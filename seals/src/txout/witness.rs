// Bitcoin protocol single-use-seals library.
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2019-2023 by
//     Dr Maxim Orlovsky <orlovsky@lnp-bp.org>
//
// Copyright (C) 2019-2023 LNP/BP Standards Association. All rights reserved.
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

use bc::{Tx, Txid};
use commit_verify::mpc;
use dbc::Proof;
use single_use_seals::SealWitness;

use crate::txout::{TxoSeal, VerifyError};

/// Witness of a seal being closed.
pub struct Witness {
    /// Witness transaction: transaction which contains commitment to the
    /// message over which the seal is closed.
    pub tx: Tx,

    /// Txid of the witness transaction.
    pub txid: Txid,

    /// Multi-protocol commitment proof from MPC anchor.
    pub proof: Proof,
}

impl<Seal: TxoSeal> SealWitness<Seal> for Witness {
    type Message = mpc::Commitment;
    type Error = VerifyError;

    fn verify_seal(&self, seal: &Seal, msg: &Self::Message) -> Result<bool, Self::Error> {
        // 1. The seal must match tx inputs
        let outpoint = seal.outpoint_or(self.txid);
        if !self
            .tx
            .inputs
            .iter()
            .any(|txin| txin.prev_output == outpoint)
        {
            return Err(VerifyError::WitnessNotClosingSeal(self.txid, outpoint));
        }

        // 2. Verify DBC with the giving closing method
        self.proof.verify(msg, &self.tx).map_err(VerifyError::from)
    }

    fn verify_many_seals<'seal>(
        &self,
        seals: impl IntoIterator<Item = &'seal Seal>,
        msg: &Self::Message,
    ) -> Result<bool, Self::Error>
    where
        Seal: 'seal,
    {
        let mut method = None;
        for seal in seals {
            // 1. All seals must have the same closing method
            if let Some(method) = method {
                if method != seal.method() {
                    return Err(VerifyError::InconsistentCloseMethod);
                }
            } else {
                method = Some(seal.method());
            }

            // 2. Each seal must match tx inputs
            let outpoint = seal.outpoint_or(self.txid);
            if !self
                .tx
                .inputs
                .iter()
                .any(|txin| txin.prev_output == outpoint)
            {
                return Err(VerifyError::WitnessNotClosingSeal(self.txid, outpoint));
            }
        }

        // 3. Verify DBC with the giving closing method
        self.proof.verify(msg, &self.tx).map_err(VerifyError::from)
    }
}
