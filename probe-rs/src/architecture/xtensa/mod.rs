//! All the interface bits for Xtensa.

use std::time::Duration;

use probe_rs_target::{Architecture, CoreType, InstructionSet};

use crate::{
    architecture::xtensa::{
        arch::{Register, SpecialRegister},
        communication_interface::DebugCause,
        registers::{FP, PC, RA, SP, XTENSA_CORE_REGSISTERS},
    },
    core::registers::{CoreRegisters, RegisterId, RegisterValue},
    BreakpointCause, CoreInformation, CoreInterface, CoreRegister, CoreStatus, Error, HaltReason,
    MemoryInterface,
};

use self::communication_interface::XtensaCommunicationInterface;

pub mod arch; // TODO: this module probably shouldn't be public but it's used in the example
mod xdm;

pub mod communication_interface;
pub(crate) mod registers;
pub(crate) mod sequences;

#[derive(Debug)]
/// Flags used to control the [`SpecificCoreState`](crate::core::SpecificCoreState) for Xtensa
/// architecture.
pub struct XtensaState {
    breakpoints_enabled: bool,
    breakpoint_set: [bool; 2],

    /// Whether the PC was written since we last halted. Used to avoid incrementing the PC on
    /// resume.
    pc_written: bool,
}

impl XtensaState {
    /// Creates a new [`XtensaState`].
    pub(crate) fn new() -> Self {
        Self {
            breakpoints_enabled: false,
            breakpoint_set: [false; 2],
            pc_written: false,
        }
    }

    fn breakpoint_mask(&self) -> u32 {
        self.breakpoint_set
            .iter()
            .enumerate()
            .fold(0, |acc, (i, &set)| if set { acc | (1 << i) } else { acc })
    }
}

/// An interface to operate Xtensa cores.
pub struct Xtensa<'probe> {
    interface: &'probe mut XtensaCommunicationInterface,
    state: &'probe mut XtensaState,
    id: usize,
}

impl<'probe> Xtensa<'probe> {
    const IBREAKA_REGS: [SpecialRegister; 2] =
        [SpecialRegister::IBreakA0, SpecialRegister::IBreakA1];

    /// Create a new Xtensa interface.
    pub fn new(
        interface: &'probe mut XtensaCommunicationInterface,
        state: &'probe mut XtensaState,
        id: usize,
    ) -> Self {
        Self {
            interface,
            id,
            state,
        }
    }

    fn core_info(&mut self) -> Result<CoreInformation, Error> {
        let pc = self.read_core_reg(self.program_counter().into())?;

        Ok(CoreInformation { pc: pc.try_into()? })
    }

    fn skip_breakpoint_instruction(&mut self) -> Result<(), Error> {
        if !self.state.pc_written {
            let debug_cause = self.interface.read_register::<DebugCause>()?;

            let pc_increment = if debug_cause.break_instruction() {
                3
            } else if debug_cause.break_n_instruction() {
                2
            } else {
                0
            };

            if pc_increment > 0 {
                // Step through the breakpoint
                let mut pc = self.read_core_reg(self.program_counter().into())?;

                pc.increment_address(pc_increment)?;

                self.write_core_reg(self.program_counter().into(), pc)?;
            }
        }

        Ok(())
    }
}

impl<'probe> MemoryInterface for Xtensa<'probe> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.interface.supports_native_64bit_access()
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        self.interface.read_word_64(address)
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        self.interface.read_word_32(address)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        self.interface.read_word_8(address)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        self.interface.read_64(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        self.interface.read_32(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.interface.read_8(address, data)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        self.interface.write_word_64(address, data)
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        self.interface.write_word_32(address, data)
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        self.interface.write_word_8(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        self.interface.write_64(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        self.interface.write_32(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.interface.write_8(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.interface.write(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        self.interface.supports_8bit_transfers()
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.interface.flush()
    }
}

impl<'probe> CoreInterface for Xtensa<'probe> {
    fn id(&self) -> usize {
        self.id
    }

    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        self.interface.wait_for_core_halted(timeout)?;
        self.state.pc_written = false;

        let status = self.status()?;

        tracing::debug!("Core halted: {:#?}", status);

        Ok(())
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        Ok(self.interface.is_halted()?)
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        if self.interface.is_halted()? {
            let debug_cause = self.interface.read_register::<DebugCause>()?;

            let is_icount_exception = debug_cause.icount_exception();
            let is_ibreak_exception = debug_cause.ibreak_exception();
            let is_break_instruction = debug_cause.break_instruction();
            let is_break_n_instruction = debug_cause.break_n_instruction();
            let is_dbreak_exception = debug_cause.dbreak_exception();
            let is_debug_interrupt = debug_cause.debug_interrupt();

            let count = is_icount_exception as u8
                + is_ibreak_exception as u8
                + is_break_instruction as u8
                + is_break_n_instruction as u8
                + is_dbreak_exception as u8
                + is_debug_interrupt as u8;

            if count > 1 {
                return Ok(CoreStatus::Halted(HaltReason::Multiple));
            }

            if is_icount_exception {
                return Ok(CoreStatus::Halted(HaltReason::Step));
            }

            if is_ibreak_exception {
                return Ok(CoreStatus::Halted(HaltReason::Breakpoint(
                    BreakpointCause::Hardware,
                )));
            }

            if is_break_instruction || is_break_n_instruction {
                return Ok(CoreStatus::Halted(HaltReason::Breakpoint(
                    BreakpointCause::Software,
                )));
            }

            if is_dbreak_exception {
                return Ok(CoreStatus::Halted(HaltReason::Watchpoint));
            }

            if is_debug_interrupt {
                return Ok(CoreStatus::Halted(HaltReason::Request));
            }

            Ok(CoreStatus::Halted(HaltReason::Unknown))
        } else {
            Ok(CoreStatus::Running)
        }
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.interface.halt()?;
        self.interface.wait_for_core_halted(timeout)?;

        self.core_info()
    }

    fn run(&mut self) -> Result<(), Error> {
        self.skip_breakpoint_instruction()?;
        Ok(self.interface.resume()?)
    }

    fn reset(&mut self) -> Result<(), Error> {
        Ok(self.interface.reset()?)
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.interface.reset_and_halt(timeout)?;

        self.core_info()
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        self.skip_breakpoint_instruction()?;
        self.interface.step()?;
        self.state.pc_written = false;

        self.core_info()
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        let register = Register::try_from(address)?;
        let value = self.interface.read_register_untyped(register)?;

        Ok(RegisterValue::U32(value))
    }

    fn write_core_reg(&mut self, address: RegisterId, value: RegisterValue) -> Result<(), Error> {
        let value: u32 = value.try_into()?;

        if address == self.program_counter().id {
            self.state.pc_written = true;
        }

        let register = Register::try_from(address)?;
        self.interface.write_register_untyped(register, value)?;

        Ok(())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        Ok(self.interface.available_breakpoint_units())
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        let mut breakpoints = Vec::with_capacity(self.available_breakpoint_units()? as usize);

        let enabled_breakpoints = self
            .interface
            .read_register_untyped(Register::Special(SpecialRegister::IBreakEnable))?;

        for i in 0..self.available_breakpoint_units()? as usize {
            let is_enabled = enabled_breakpoints & (1 << i) != 0;
            let breakpoint = if is_enabled {
                let address = self
                    .interface
                    .read_register_untyped(Register::Special(Self::IBREAKA_REGS[i]))?;

                Some(address as u64)
            } else {
                None
            };

            breakpoints.push(breakpoint)
        }

        Ok(breakpoints)
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        self.state.breakpoints_enabled = state;
        let mask = self.state.breakpoint_mask();

        self.interface
            .write_register_untyped(SpecialRegister::IBreakEnable, if state { mask } else { 0 })?;

        Ok(())
    }

    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        self.state.breakpoint_set[unit_index] = true;
        self.interface
            .write_register_untyped(Self::IBREAKA_REGS[unit_index], addr as u32)?;

        if self.state.breakpoints_enabled {
            let mask = self.state.breakpoint_mask();
            self.interface
                .write_register_untyped(SpecialRegister::IBreakEnable, mask)?;
        }

        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        self.state.breakpoint_set[unit_index] = false;

        if self.state.breakpoints_enabled {
            let mask = self.state.breakpoint_mask();
            self.interface
                .write_register_untyped(SpecialRegister::IBreakEnable, mask)?;
        }

        Ok(())
    }

    fn registers(&self) -> &'static CoreRegisters {
        &XTENSA_CORE_REGSISTERS
    }

    fn program_counter(&self) -> &'static CoreRegister {
        &PC
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        &FP
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        &SP
    }

    fn return_address(&self) -> &'static CoreRegister {
        &RA
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        self.state.breakpoints_enabled
    }

    fn architecture(&self) -> Architecture {
        Architecture::Xtensa
    }

    fn core_type(&self) -> CoreType {
        CoreType::Xtensa
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        // TODO: NX exists, too
        Ok(InstructionSet::Xtensa)
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        // TODO: ESP32 and ESP32-S3 have FPU
        Ok(false)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        // TODO: ESP32 and ESP32-S3 have FPU
        Ok(0)
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        Ok(self.interface.halt_on_reset(true)?)
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        Ok(self.interface.halt_on_reset(false)?)
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.interface.leave_ocd_mode()?;
        Ok(())
    }
}
