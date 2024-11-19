// UltraSONIC: transactional execution layer with capability-based memory access for zk-AluVM
//
// SPDX-License-Identifier: Apache-2.0
//
// Designed in 2019-2025 by Dr Maxim Orlovsky <orlovsky@ubideco.org>
// Written in 2024-2025 by Dr Maxim Orlovsky <orlovsky@ubideco.org>
//
// Copyright (C) 2019-2024 LNP/BP Standards Association, Switzerland.
// Copyright (C) 2024-2025 Laboratories for Ubiquitous Deterministic Computing (UBIDECO),
//                         Institute for Distributed and Cognitive Systems (InDCS), Switzerland.
// Copyright (C) 2019-2025 Dr Maxim Orlovsky.
// All rights under the above copyrights are reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use this file except
// in compliance with the License. You may obtain a copy of the License at
//
//        http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software distributed under the License
// is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express
// or implied. See the License for the specific language governing permissions and limitations under
// the License.

use aluvm::regs::Status;
use aluvm::{fe128, CoreConfig, CoreExt, Lib, LibId, LibSite, RegE, Vm};
use amplify::confinement::{SmallString, SmallVec, TinyOrdMap, TinyString};
use commit_verify::ReservedBytes;

use crate::{CellAddr, ContractId, Instr, Operation, StateCell, StateData, LIB_NAME_ULTRASONIC};

pub type CallId = u16;
pub type AccessId = u16;

/// Codex is a crucial part of a contract; it provides a set of commitments to the contract terms
/// and conditions expressed as a deterministic program able to run in SONIC computer model.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[derive(StrictType, StrictDumb, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_ULTRASONIC)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "camelCase"))]
pub struct Codex {
    pub name: TinyString,
    pub developer: SmallString,
    pub version: ReservedBytes<2>,
    pub field_order: u128,
    pub input_config: CoreConfig,
    pub verification_config: CoreConfig,
    pub verifiers: TinyOrdMap<CallId, LibSite>,
    /// Reserved for the future codex extensions
    pub reserved: ReservedBytes<8>,
}

impl Codex {
    pub fn verify(
        &self,
        contract_id: ContractId,
        operation: &Operation,
        memory: &impl Memory,
        repo: &impl LibRepo,
    ) -> Result<(), CallError> {
        let resolver = |lib_id: LibId| repo.get_lib(lib_id);

        if operation.contract_id != contract_id {
            return Err(CallError::WrongContract {
                expected: contract_id,
                found: operation.contract_id,
            });
        }

        // Phase one: get inputs, verify access conditions
        let mut vm_inputs =
            Vm::<aluvm::gfa::Instr<LibId>>::with(self.input_config, self.field_order);
        let mut read_once_input = SmallVec::new();
        for input in &operation.destroying {
            let cell = memory
                .read_once(input.addr)
                .ok_or(CallError::NoReadOnceInput(input.addr))?;

            // Verify that the lock script conditions are satisfied
            if let Some(lock) = cell.lock {
                // Put witness into input registers
                for (no, reg) in [RegE::E1, RegE::E2, RegE::E3, RegE::E4]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(el) = input.witness.get(no as u8) {
                        vm_inputs.core.cx.set(reg, el);
                    }
                }
                if vm_inputs.exec(lock, &(), resolver) == Status::Fail {
                    // Read error code from output register
                    return Err(CallError::Lock(vm_inputs.core.cx.get(RegE::E8)));
                }
                vm_inputs.reset();
            }

            let _ = read_once_input.push(cell.data);
        }

        let mut immutable_input = SmallVec::new();
        for input in &operation.destroying {
            let data = memory
                .immutable(input.addr)
                .ok_or(CallError::NoImmutableInput(input.addr))?;
            let _ = immutable_input.push(data);
        }

        // Phase 2: Verify operation integrity
        let entry_point = self
            .verifiers
            .get(&operation.call_id)
            .ok_or(CallError::NotFound(operation.call_id))?;
        let context = VmContext {
            read_once_input: read_once_input.as_slice(),
            immutable_input: immutable_input.as_slice(),
            read_once_output: operation.destructible.as_slice(),
            immutable_output: operation.immutable.as_slice(),
        };
        let mut vm_main = Vm::<Instr<LibId>>::with(self.verification_config, self.field_order);
        match vm_main.exec(*entry_point, &context, resolver) {
            Status::Ok => Ok(()),
            Status::Fail => {
                if let Some(err_code) = vm_main.core.cx.get(RegE::E1) {
                    Err(CallError::Script(err_code))
                } else {
                    Err(CallError::ScriptUnspecified)
                }
            }
        }
    }
}

pub trait Memory {
    fn read_once(&self, addr: CellAddr) -> Option<StateCell>;
    fn immutable(&self, addr: CellAddr) -> Option<StateData>;
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VmContext<'ctx> {
    pub read_once_input: &'ctx [StateData],
    pub immutable_input: &'ctx [StateData],
    pub read_once_output: &'ctx [StateCell],
    pub immutable_output: &'ctx [StateData],
}

pub trait LibRepo {
    fn get_lib(&self, lib_id: LibId) -> Option<&Lib>;
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Display, Error)]
#[display(doc_comments)]
pub enum CallError {
    /// operation doesn't belong to the current contract {expected} (operation contract is
    /// {found}).
    WrongContract {
        expected: ContractId,
        found: ContractId,
    },
    /// operation verifier {0} is not present in the codex.
    NotFound(CallId),
    /// operation references read-once memory cell {0} which was not defined.
    NoReadOnceInput(CellAddr),
    /// operation references immutable memory cell {0} which was not defined.
    NoImmutableInput(CellAddr),
    /// operation input locking conditions are unsatisfied.
    Lock(Option<fe128>),
    /// verification failure {0}
    Script(fe128),
    /// verification failure (details are unspecified).
    ScriptUnspecified,
}
