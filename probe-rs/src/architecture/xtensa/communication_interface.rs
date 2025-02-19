//! Xtensa Debug Module Communication

// TODO: remove
#![allow(missing_docs)]

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::{
    architecture::xtensa::arch::{
        instruction::Instruction, CpuRegister, Register, SpecialRegister,
    },
    probe::JTAGAccess,
    DebugProbeError, Error as ProbeRsError, MemoryInterface,
};

use super::xdm::{Error as XdmError, Xdm};

/// Possible Xtensa errors
#[derive(thiserror::Error, Debug)]
pub enum XtensaError {
    /// An error originating from the DebugProbe
    #[error("Debug Probe Error")]
    DebugProbe(#[from] DebugProbeError),
    /// Xtensa debug module error
    #[error("Xtensa debug module error")]
    XdmError(#[from] XdmError),
    /// A timeout occurred
    // TODO: maybe we could be a bit more specific
    #[error("The operation has timed out")]
    Timeout,
    /// The connected target is not an Xtensa device.
    #[error("Connected target is not an Xtensa device.")]
    NoXtensaTarget,
    /// The requested register is not available.
    #[error("The requested register is not available.")]
    RegisterNotAvailable,
}

impl From<XtensaError> for DebugProbeError {
    fn from(e: XtensaError) -> DebugProbeError {
        match e {
            XtensaError::DebugProbe(err) => err,
            other_error => DebugProbeError::Other(other_error.into()),
        }
    }
}

impl From<XtensaError> for ProbeRsError {
    fn from(err: XtensaError) -> Self {
        match err {
            XtensaError::DebugProbe(e) => e.into(),
            XtensaError::Timeout => ProbeRsError::Timeout,
            other => ProbeRsError::Xtensa(other),
        }
    }
}

#[derive(Clone, Copy)]
#[allow(unused)]
pub(super) enum DebugLevel {
    L2 = 2,
    L3 = 3,
    L4 = 4,
    L5 = 5,
    L6 = 6,
    L7 = 7,
}

impl DebugLevel {
    pub fn pc(self) -> SpecialRegister {
        match self {
            DebugLevel::L2 => SpecialRegister::Epc2,
            DebugLevel::L3 => SpecialRegister::Epc3,
            DebugLevel::L4 => SpecialRegister::Epc4,
            DebugLevel::L5 => SpecialRegister::Epc5,
            DebugLevel::L6 => SpecialRegister::Epc6,
            DebugLevel::L7 => SpecialRegister::Epc7,
        }
    }

    pub fn ps(self) -> SpecialRegister {
        match self {
            DebugLevel::L2 => SpecialRegister::Eps2,
            DebugLevel::L3 => SpecialRegister::Eps3,
            DebugLevel::L4 => SpecialRegister::Eps4,
            DebugLevel::L5 => SpecialRegister::Eps5,
            DebugLevel::L6 => SpecialRegister::Eps6,
            DebugLevel::L7 => SpecialRegister::Eps7,
        }
    }
}

struct XtensaCommunicationInterfaceState {
    /// Pairs of (register, value).
    saved_registers: HashMap<Register, u32>,

    print_exception_cause: bool,

    is_halted: bool,
}

/// A interface that implements controls for Xtensa cores.
#[allow(unused)] // TODO: remove
pub struct XtensaCommunicationInterface {
    /// The Xtensa debug module
    xdm: Xdm,
    state: XtensaCommunicationInterfaceState,

    hw_breakpoint_num: u32,
    debug_level: DebugLevel,
}

impl XtensaCommunicationInterface {
    /// Create the Xtensa communication interface using the underlying probe driver
    pub fn new(probe: Box<dyn JTAGAccess>) -> Result<Self, (Box<dyn JTAGAccess>, DebugProbeError)> {
        let xdm = Xdm::new(probe).map_err(|(probe, e)| (probe, e.into()))?;

        let mut s = Self {
            xdm,
            state: XtensaCommunicationInterfaceState {
                saved_registers: Default::default(),
                print_exception_cause: true,
                is_halted: false,
            },
            // TODO chip-specific configuration
            hw_breakpoint_num: 2,
            debug_level: DebugLevel::L6,
        };

        match s.init() {
            Ok(()) => Ok(s),

            Err(e) => Err((s.xdm.free(), e.into())),
        }
    }

    fn init(&mut self) -> Result<(), XtensaError> {
        // TODO any initialization that needs to be done
        Ok(())
    }

    pub fn available_breakpoint_units(&self) -> u32 {
        self.hw_breakpoint_num
    }

    pub fn halt_on_reset(&mut self, en: bool) -> Result<(), XtensaError> {
        self.xdm.halt_on_reset(en);
        Ok(())
    }

    pub fn enter_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.xdm.halt()?;
        tracing::info!("Entered OCD mode");
        Ok(())
    }

    pub fn is_in_ocd_mode(&mut self) -> Result<bool, XtensaError> {
        self.xdm.is_in_ocd_mode()
    }

    pub fn leave_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.restore_registers()?;
        self.resume()?;
        self.xdm.leave_ocd_mode()?;
        tracing::info!("Left OCD mode");
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), XtensaError> {
        match self.reset_and_halt(Duration::from_millis(500)) {
            Ok(_) => {
                self.resume()?;

                Ok(())
            }
            Err(error) => Err(XtensaError::DebugProbe(DebugProbeError::Other(
                anyhow::anyhow!("Error during reset: {:?}", error),
            ))),
        }
    }

    pub fn reset_and_halt(&mut self, timeout: Duration) -> Result<(), XtensaError> {
        self.xdm.target_reset_assert()?;
        self.xdm.halt_on_reset(true);
        self.xdm.target_reset_deassert()?;
        self.wait_for_core_halted(timeout)?;
        self.xdm.halt_on_reset(false);

        // TODO: this is only necessary to run code, so this might not be the best place
        self.write_register_untyped(Register::CurrentPs, 0x40021)?;

        Ok(())
    }

    pub fn halt(&mut self) -> Result<(), XtensaError> {
        tracing::debug!("Halting core");
        self.xdm.halt()
    }

    pub fn is_halted(&mut self) -> Result<bool, XtensaError> {
        self.xdm.is_halted()
    }

    pub fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), XtensaError> {
        let now = Instant::now();
        while !self.is_halted()? {
            if now.elapsed() > timeout {
                return Err(XtensaError::Timeout);
            }

            std::thread::sleep(Duration::from_millis(1));
        }
        tracing::debug!("Core halted");
        self.state.is_halted = true;

        // Force a low INTLEVEL
        // TODO: do this only if we set a breakpoint or watchpoint or single step
        let old_ps = self.read_register_untyped(Register::CurrentPs)?;
        self.write_register_untyped(Register::CurrentPs, (old_ps & !0xF) | 0x1)?;

        Ok(())
    }

    pub fn step(&mut self) -> Result<(), XtensaError> {
        self.write_register_untyped(
            Register::Special(SpecialRegister::ICountLevel),
            self.debug_level as u32,
        )?;

        // An exception is generated at the beginning of an instruction that would overflow ICOUNT.
        self.write_register_untyped(Register::Special(SpecialRegister::ICount), -2_i32 as u32)?;

        self.resume()?;
        self.wait_for_core_halted(Duration::from_millis(100))?;

        // Avoid stopping again
        self.write_register_untyped(
            Register::Special(SpecialRegister::ICount),
            self.debug_level as u32 + 1,
        )?;

        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), XtensaError> {
        tracing::debug!("Resuming core");
        self.state.is_halted = false;
        self.xdm.resume()?;

        Ok(())
    }

    fn read_cpu_register(&mut self, register: CpuRegister) -> Result<u32, XtensaError> {
        self.execute_instruction(Instruction::Wsr(SpecialRegister::Ddr, register))?;
        self.xdm.read_ddr()
    }

    fn read_special_register(&mut self, register: SpecialRegister) -> Result<u32, XtensaError> {
        let save_key = self.save_register(CpuRegister::A3)?;

        // Read special register into the scratch register
        self.execute_instruction(Instruction::Rsr(register, CpuRegister::A3))?;

        // Read the scratch register
        let result = self.read_cpu_register(CpuRegister::A3)?;

        self.restore_register(save_key)?;

        Ok(result)
    }

    fn write_special_register(
        &mut self,
        register: SpecialRegister,
        value: u32,
    ) -> Result<(), XtensaError> {
        tracing::debug!("Writing special register: {:?}", register);
        let save_key = self.save_register(CpuRegister::A3)?;

        self.xdm.write_ddr(value)?;

        // DDR -> scratch
        self.xdm
            .execute_instruction(Instruction::Rsr(SpecialRegister::Ddr, CpuRegister::A3))?;

        // scratch -> target special register
        self.xdm
            .execute_instruction(Instruction::Wsr(register, CpuRegister::A3))?;

        self.restore_register(save_key)?;

        Ok(())
    }

    fn write_cpu_register(&mut self, register: CpuRegister, value: u32) -> Result<(), XtensaError> {
        tracing::debug!("Writing {:x} to register: {:?}", value, register);

        self.xdm.write_ddr(value)?;
        self.xdm
            .execute_instruction(Instruction::Rsr(SpecialRegister::Ddr, register))?;

        Ok(())
    }

    fn debug_execution_error_impl(&mut self, status: XdmError) -> Result<(), XtensaError> {
        if let XdmError::ExecExeception = status {
            if !self.state.print_exception_cause {
                tracing::warn!("Instruction exception while reading previous exception");
                return Ok(());
            }

            tracing::warn!("Failed to execute instruction, attempting to read debug info");

            // clear ExecException to allow new instructions to run
            self.xdm.clear_exec_exception()?;

            for (name, reg) in [
                ("EXCCAUSE", SpecialRegister::ExcCause),
                ("EXCVADDR", SpecialRegister::ExcVaddr),
                ("DEBUGCAUSE", SpecialRegister::DebugCause),
            ] {
                let register = self.read_register_untyped(reg)?;

                tracing::info!("{}: {:08x}", name, register);
            }
        }

        Ok(())
    }

    fn debug_execution_error(&mut self, status: XdmError) -> Result<(), XtensaError> {
        self.state.print_exception_cause = false;
        let result = self.debug_execution_error_impl(status);
        self.state.print_exception_cause = true;

        result
    }

    fn execute_instruction(&mut self, inst: Instruction) -> Result<(), XtensaError> {
        let status = self.xdm.execute_instruction(inst);
        if let Err(XtensaError::XdmError(err)) = status {
            self.debug_execution_error(err)?
        }
        status
    }

    fn read_ddr_and_execute(&mut self) -> Result<u32, XtensaError> {
        let status = self.xdm.read_ddr_and_execute();
        if let Err(XtensaError::XdmError(err)) = status {
            self.debug_execution_error(err)?
        }
        status
    }

    fn write_ddr_and_execute(&mut self, value: u32) -> Result<(), XtensaError> {
        let status = self.xdm.write_ddr_and_execute(value);
        if let Err(XtensaError::XdmError(err)) = status {
            self.debug_execution_error(err)?
        }
        status
    }

    pub fn read_register<R: TypedRegister>(&mut self) -> Result<R, XtensaError> {
        let value = self.read_register_untyped(R::register())?;

        Ok(R::from_u32(value))
    }

    pub fn read_register_untyped(
        &mut self,
        register: impl Into<Register>,
    ) -> Result<u32, XtensaError> {
        match register.into() {
            Register::Cpu(register) => self.read_cpu_register(register),
            Register::Special(register) => self.read_special_register(register),
            Register::CurrentPc => self.read_special_register(self.debug_level.pc()),
            Register::CurrentPs => self.read_special_register(self.debug_level.ps()),
        }
    }

    pub fn write_register_untyped(
        &mut self,
        register: impl Into<Register>,
        value: u32,
    ) -> Result<(), XtensaError> {
        match register.into() {
            Register::Cpu(register) => self.write_cpu_register(register, value),
            Register::Special(register) => self.write_special_register(register, value),
            Register::CurrentPc => self.write_special_register(self.debug_level.pc(), value),
            Register::CurrentPs => self.write_special_register(self.debug_level.ps(), value),
        }
    }

    pub fn save_register(
        &mut self,
        register: impl Into<Register>,
    ) -> Result<Option<Register>, XtensaError> {
        let register = register.into();

        if matches!(
            register,
            Register::Special(
                SpecialRegister::Ddr | SpecialRegister::ICount | SpecialRegister::ICountLevel
            )
        ) {
            // Avoid saving some registers
            return Ok(None);
        }

        let is_saved = self.state.saved_registers.contains_key(&register);

        if is_saved {
            return Ok(None);
        }

        tracing::debug!("Saving register: {:?}", register);
        let value = self.read_register_untyped(register)?;
        self.state.saved_registers.insert(register, value);

        Ok(Some(register))
    }

    fn restore_register(&mut self, key: Option<Register>) -> Result<(), XtensaError> {
        let Some(key) = key else {
            return Ok(());
        };

        tracing::debug!("Restoring register: {:?}", key);

        if let Some(value) = self.state.saved_registers.get(&key) {
            self.write_register_untyped(key, *value)?;

            self.state.saved_registers.remove(&key);
        }

        Ok(())
    }

    fn restore_registers(&mut self) -> Result<(), XtensaError> {
        tracing::debug!("Restoring registers");

        // Clone the list of saved registers so we can iterate over it, but code may still save
        // new registers. We can't take it otherwise the restore loop would unnecessarily save
        // registers.
        // Currently, restoring registers may only use the scratch register which is already saved
        // if we access special registers. This means the register list won't actually change in the
        // next loop.
        let dirty_regs = self.state.saved_registers.clone();

        let mut restore_scratch = None;

        for (register, value) in dirty_regs.iter().map(|(k, v)| (*k, *v)) {
            if register == Register::Cpu(CpuRegister::A3) {
                // We need to handle the scratch register (A3) separately as restoring a special
                // register will overwrite it.
                restore_scratch = Some(value);
            } else {
                self.write_register_untyped(register, value)?;
            }
        }

        if self.state.saved_registers.len() != dirty_regs.len() {
            // The scratch register wasn't saved before, but has to be saved now. This case should
            // not currently be reachable.
            restore_scratch = self
                .state
                .saved_registers
                .get(&Register::Cpu(CpuRegister::A3))
                .copied();
        }

        if let Some(value) = restore_scratch {
            self.write_register_untyped(CpuRegister::A3, value)?;
        }

        self.state.saved_registers.clear();

        Ok(())
    }

    fn read_memory(&mut self, address: u64, mut dst: &mut [u8]) -> Result<(), XtensaError> {
        tracing::debug!("Reading {} bytes from address {:08x}", dst.len(), address);
        if dst.is_empty() {
            return Ok(());
        }

        // Write aligned address to the scratch register
        let key = self.save_register(CpuRegister::A3)?;
        self.write_cpu_register(CpuRegister::A3, address as u32 & !0x3)?;

        // Read from address in the scratch register
        self.execute_instruction(Instruction::Lddr32P(CpuRegister::A3))?;

        // Let's assume we can just do 32b reads, so let's do some pre-massaging on unaligned reads
        if address % 4 != 0 {
            let offset = address as usize % 4;

            // Avoid executing another read if we only have to read a single word
            let word = if offset + dst.len() <= 4 {
                self.xdm.read_ddr()?
            } else {
                self.read_ddr_and_execute()?
            };

            let word = word.to_le_bytes();

            let bytes_to_copy = (4 - offset).min(dst.len());

            dst[..bytes_to_copy].copy_from_slice(&word[offset..][..bytes_to_copy]);
            dst = &mut dst[bytes_to_copy..];

            if dst.is_empty() {
                return Ok(());
            }
        }

        while dst.len() > 4 {
            let word = self.read_ddr_and_execute()?.to_le_bytes();
            dst[..4].copy_from_slice(&word);
            dst = &mut dst[4..];
        }

        let remaining_bytes = dst.len();

        let word = self.xdm.read_ddr()?;
        dst.copy_from_slice(&word.to_le_bytes()[..remaining_bytes]);

        self.restore_register(key)?;

        Ok(())
    }

    fn write_memory_unaligned8(&mut self, address: u32, data: &[u8]) -> Result<(), XtensaError> {
        if data.is_empty() {
            return Ok(());
        }

        let key = self.save_register(CpuRegister::A3)?;

        let offset = address as usize % 4;
        let aligned_address = address & !0x3;

        // Read the aligned word
        let mut word = [0; 4];
        self.read_memory(aligned_address as u64, &mut word)?;

        // Replace the written bytes. This will also panic if the input is crossing a word boundary
        word[offset..][..data.len()].copy_from_slice(data);

        // Write the word back
        self.write_register_untyped(CpuRegister::A3, aligned_address)?;
        self.xdm.write_ddr(u32::from_le_bytes(word))?;
        self.execute_instruction(Instruction::Sddr32P(CpuRegister::A3))?;
        self.restore_register(key)?;

        Ok(())
    }

    fn write_memory(&mut self, address: u64, data: &[u8]) -> Result<(), XtensaError> {
        tracing::debug!("Writing {} bytes to address {:08x}", data.len(), address);
        if data.is_empty() {
            return Ok(());
        }

        let key = self.save_register(CpuRegister::A3)?;

        let address = address as u32;

        let mut addr = address;
        let mut buffer = data;

        // We store the unaligned head of the data separately
        if addr % 4 != 0 {
            let unaligned_bytes = (4 - (addr % 4) as usize).min(buffer.len());

            self.write_memory_unaligned8(addr, &buffer[..unaligned_bytes])?;

            buffer = &buffer[unaligned_bytes..];
            addr += unaligned_bytes as u32;
        }

        if buffer.len() > 4 {
            // Prepare store instruction
            self.save_register(CpuRegister::A3)?;
            self.write_register_untyped(CpuRegister::A3, addr)?;

            self.xdm
                .write_instruction(Instruction::Sddr32P(CpuRegister::A3))?;

            while buffer.len() > 4 {
                let mut word = [0; 4];
                word[..].copy_from_slice(&buffer[..4]);
                let word = u32::from_le_bytes(word);

                // Write data to DDR and store
                self.write_ddr_and_execute(word)?;

                buffer = &buffer[4..];
                addr += 4;
            }
        }

        // We store the narrow tail of the data separately
        if !buffer.is_empty() {
            self.write_memory_unaligned8(addr, buffer)?;
        }

        self.restore_register(key)?;

        // TODO: implement cache flushing on CPUs that need it.

        Ok(())
    }
}

/// DataType
///
/// # Safety
/// Don't implement this trait
unsafe trait DataType: Sized {}
unsafe impl DataType for u8 {}
unsafe impl DataType for u32 {}
unsafe impl DataType for u64 {}

fn as_bytes<T: DataType>(data: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *mut u8, std::mem::size_of_val(data)) }
}

fn as_bytes_mut<T: DataType>(data: &mut [T]) -> &mut [u8] {
    unsafe {
        std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, std::mem::size_of_val(data))
    }
}

impl MemoryInterface for XtensaCommunicationInterface {
    fn read(&mut self, address: u64, dst: &mut [u8]) -> Result<(), crate::Error> {
        self.read_memory(address, dst)?;

        Ok(())
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, crate::Error> {
        let mut out = [0; 4];
        self.read(address, &mut out)?;

        Ok(u32::from_le_bytes(out))
    }

    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, address: u64) -> anyhow::Result<u64, crate::Error> {
        let mut out = [0; 8];
        self.read(address, &mut out)?;

        Ok(u64::from_le_bytes(out))
    }

    fn read_word_8(&mut self, address: u64) -> anyhow::Result<u8, crate::Error> {
        let mut out = 0;
        self.read(address, std::slice::from_mut(&mut out))?;
        Ok(out)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> anyhow::Result<(), crate::Error> {
        self.read_8(address, as_bytes_mut(data))
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> anyhow::Result<(), crate::Error> {
        self.read_8(address, as_bytes_mut(data))
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> anyhow::Result<(), crate::Error> {
        self.read(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), crate::Error> {
        self.write_memory(address, data)?;

        Ok(())
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> anyhow::Result<(), crate::Error> {
        self.write(address, &data.to_le_bytes())
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> anyhow::Result<(), crate::Error> {
        self.write(address, &data.to_le_bytes())
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> anyhow::Result<(), crate::Error> {
        self.write(address, &[data])
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> anyhow::Result<(), crate::Error> {
        self.write_8(address, as_bytes(data))
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> anyhow::Result<(), crate::Error> {
        self.write_8(address, as_bytes(data))
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> anyhow::Result<(), crate::Error> {
        self.write(address, data)
    }

    fn supports_8bit_transfers(&self) -> anyhow::Result<bool, crate::Error> {
        Ok(true)
    }

    fn flush(&mut self) -> anyhow::Result<(), crate::Error> {
        Ok(())
    }
}

pub trait TypedRegister {
    fn register() -> Register;
    fn from_u32(value: u32) -> Self;
}

bitfield::bitfield! {
    #[derive(Copy, Clone)]
    pub struct DebugCause(u32);
    impl Debug;

    pub icount_exception,    set_icount_exception   : 0;
    pub ibreak_exception,    set_ibreak_exception   : 1;
    pub dbreak_exception,    set_dbreak_exception   : 2;
    pub break_instruction,   set_break_instruction  : 3;
    pub break_n_instruction, set_break_n_instruction: 4;
    pub debug_interrupt,     set_debug_interrupt    : 5;
    pub dbreak_num,          set_dbreak_num         : 11, 8;
}

impl TypedRegister for DebugCause {
    fn register() -> Register {
        Register::Special(SpecialRegister::DebugCause)
    }

    fn from_u32(value: u32) -> Self {
        Self(value)
    }
}
