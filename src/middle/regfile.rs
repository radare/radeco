// Copyright (c) 2015, The Radare Project. All rights reserved.
// See the COPYING file at the top-level directory of this distribution.
// Licensed under the BSD 3-Clause License:
// <http://opensource.org/licenses/BSD-3-Clause>
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contains the struct `SubRegisterFile` which extends `PhiPlacer`s
//! functionality by reads and writes to partial registers.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::convert::From;

use r2api::structs::LRegInfo;

use middle::ssa::ssa_traits::{ValueType, RegInfo};

#[derive(Clone, Copy, Debug)]
pub struct SubRegister {
    pub base: usize,
    pub shift: usize,
    pub width: usize,
}

impl SubRegister {
    fn new(base: usize, shift: usize, width: usize) -> SubRegister {
        SubRegister {
            base: base,
            shift: shift,
            width: width,
        }
    }
}

/// A structure containing information about whole and partial registers of a platform.
/// Upon creation it builds a vector of `ValueType`s representing whole registers
/// to be added to a `PhiPlacer`.
///
/// It can then translate accesses to partial registers to accesses of whole registers.
/// Shifts and masks are added automatically.
#[derive(Clone, Debug)]
pub struct SubRegisterFile {
    /// `ValueType`s of whole registers ready to be added to a `PhiPlacer`.
    /// The index within `PhiPlacer` to the first register is needed
    /// as argument `base` to `read_register` and `write_register` later.
    pub whole_registers: Vec<ValueType>,
    /// Contains the respective names for the registers described in `whole_registers`
    pub whole_names: Vec<String>,
    named_registers: HashMap<String, SubRegister>,
    /// Contains the alias information for some registers.
    pub alias_info: HashMap<String, String>,
    /// Contains the type information for every registers.
    pub type_info: HashMap<String, String>,
}

impl SubRegisterFile {
    /// Creates a new SubRegisterFile based on a provided register profile.
    pub fn new(reg_info: &LRegInfo) -> SubRegisterFile {
        let mut aliases: HashMap<String, String> = HashMap::new();
        for reg in &reg_info.alias_info {
            aliases.insert(reg.reg.clone(), reg.role_str.clone());
        }

        let mut slices = HashMap::new();
        let mut events: Vec<SubRegister> = Vec::new();
        let mut types: HashMap<String, String> = HashMap::new();
        for (i, reg) in reg_info.reg_info.iter().enumerate() {
            types.insert(reg.name.clone(), reg.type_str.clone());
            if reg.type_str == "fpu" {
                continue;
            } // st7 from "fpu" overlaps with zf from "gpr" (r2 bug?)
            if reg.name.ends_with("flags") {
                continue;
            } // HARDCODED x86
            events.push(SubRegister::new(i, reg.offset, reg.size));
        }

        events.sort_by(|a, b| {
            let o = a.shift.cmp(&b.shift);
            if let Ordering::Equal = o {
                (b.width + b.shift).cmp(&(a.width + a.shift))
            } else {
                o
            }
        });

        let mut current = SubRegister::new(0, 0, 0);
        let mut whole: Vec<ValueType> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for &ev in &events {
            let name = &reg_info.reg_info[ev.base].name;
            let cur_until = current.shift + current.width;
            if ev.shift >= cur_until {
                current = ev;

                radeco_trace!("regfile_mappings|{} -> {}", whole.len(), &name);

                whole.push(From::from(current.width));
                names.push(name.clone());
            } else {
                let ev_until = ev.width + ev.shift;
                assert!(ev_until <= cur_until);
            }

            let subreg = SubRegister::new(whole.len() - 1, ev.shift - current.shift, ev.width);

            slices.insert(name.clone(), subreg);
        }

        SubRegisterFile {
            whole_registers: whole,
            named_registers: slices,
            whole_names: names,
            alias_info: aliases,
            type_info: types,
        }
    }

    // API for Sub Reigster.
    pub fn get_subregister(&self, name: &str) -> Option<SubRegister> {
        self.named_registers.get(name).cloned()
    }

    
    // API for whole register.
    
    // Get information by id.
    pub fn get_name(&self, id: usize) -> Option<String> {
        Some(self.whole_names[id].clone())
    }

    pub fn get_width(&self, id: usize) -> Option<usize> {
        if let Some(name) = self.get_name(id) {
            if let Some(subreg) = self.named_registers.get(&name) {
                Some(subreg.width)
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn get_reginfo(&self, id: usize) -> Option<RegInfo> {
        if let Some(name) = self.get_name(id) {
            Some(RegInfo {
                name: name.clone(),
                type_info: self.type_info.get(&name).cloned(),
                alias_info: self.alias_info.get(&name).cloned(),
                width: self.get_width(id),
            })
        } else {
            None
        }
    }


    // Get information by other way. 
    pub fn get_name_by_alias(&self, alias: &String) -> Option<String> {
        for id in 0..self.whole_names.len() {
            if let Some(name) = self.get_name(id) {
                if self.alias_info.get(&name) == Some(alias) {
                    return Some(name);
                }
            }
        }
        None
    }

    // Emit code for setting the specified register to the specified value.
    // Will automatically insert code for shifting and masking in case of subregisters.
    // This implies that it also tries to read the old value of the whole register.
    //
    // # Arguments
    // * `phiplacer` - A PhiPlacer that has already been informed of our variables.
    //                 It will also give us access to the SSA to modify
    // * `base`      - Index of the PhiPlacer variable that corresponds to our first register.
    // * `block`     - Reference to the basic block to which operations will be appended.
    // * `var`       - Name of the register to write as a string.
    // * `value`     - An SSA node whose value shall be assigned to the register.
    //                 As with most APIs in radeco, we will not check if the value is reachable
    //                 from the position where the caller is trying to insert these operations.

    //pub fn write_register<'a, T>(&self,
                                 //phip: &mut PhiPlacer<'a, T>,
                                 //base: usize,
                                 //block: T::ActionRef,
                                 //var: &String,
                                 //mut value: T::ValueRef,
                                 //addr: u64)
        //where T: 'a + SSAMod<BBInfo = BBInfo> + VerifiedAdd
    //{
        //let info = &self.named_registers[var];
        //let id = info.base + base;

        //let width = match phip.variable_types[id] {
            //ValueType::Integer { width } => width,
        //};

        //if info.width >= width as usize {
            //phip.write_variable(block, id, value);
            //return;
        //}

        //// Need to add a cast.
        //let vt = From::from(width);
        //let opcode = MOpcode::OpWiden(width as WidthSpec);

        //if phip.ssa.get_node_data(&value).ok().map_or(0, |nd| {
            //match nd.vt {
                //ValueType::Integer{width} => width,
            //}
        //}) < width {
            //value = phip.ssa.verified_add_op(block, opcode, vt, &[value], Some(addr));
        //}

        //let mut new_value;

        //if info.shift > 0 {
            //let shift_amount_node = phip.add_const(block, info.shift as u64);
            //new_value = phip.ssa.verified_add_op(block,
                                                 //MOpcode::OpLsl,
                                                 //vt,
                                                 //&[value, shift_amount_node],
                                                 //Some(addr));
            //value = new_value;
        //}

        //let fullval: u64 = !((!1u64) << (width - 1));
        //let maskval: u64 = ((!((!1u64) << (info.width - 1))) << info.shift) ^ fullval;

        //if maskval == 0 {
            //phip.write_variable(block, id, value);
            //return;
        //}

        //let mut ov = phip.read_variable(block, id);
        //let maskvalue_node = phip.add_const(block, maskval);
        //new_value = phip.ssa.verified_add_op(block,
                                             //MOpcode::OpAnd,
                                             //vt,
                                             //&[ov, maskvalue_node],
                                             //Some(addr));

        //ov = new_value;
        //new_value = phip.ssa.verified_add_op(block, MOpcode::OpOr, vt, &[value, ov], Some(addr));
        //value = new_value;
        //phip.write_variable(block, id, value);
    //}

    // Emit code for reading the current value of the specified register.
    //
    // # Arguments
    // * `phiplacer` - A PhiPlacer that has already been informed of our variables.
    //                 It will also give us access to the SSA to modify
    // * `base`      - Index of the PhiPlacer variable that corresponds to our first register.
    // * `block`     - Reference to the basic block to which operations will be appended.
    // * `var`       - Name of the register to read as a string.
    //
    // # Return value
    // A SSA node representing the value of the register as seen by the current end of `block`.
    // Unless prior basic blocks are marked as sealed in the PhiPlacer this will always return
    // a reference to a Phi node.
    // Either way, once nodes are sealed redundant Phi nodes are eliminated by PhiPlacer.

    //pub fn read_register<'a, T>(&self,
                                //phiplacer: &mut PhiPlacer<'a, T>,
                                //base: usize,
                                //block: T::ActionRef,
                                //var: &String,
                                //addr: u64)
                                //-> T::ValueRef
        //where T: SSAMod<BBInfo = BBInfo> + VerifiedAdd + 'a
    //{
        //let info = &self.named_registers[var];
        //let id = info.base + base;
        //let mut value = phiplacer.read_variable(block, id);

        //let width = match phiplacer.variable_types[id] {
            //ValueType::Integer { width } => width,
        //};

        //if info.shift > 0 {
            //let shift_amount_node = phiplacer.add_const(block, info.shift as u64);
            //let opcode = MOpcode::OpLsr;
            //let vtype = From::from(width);
            //let new_value = phiplacer.ssa.verified_add_op(block,
                                                          //opcode,
                                                          //vtype,
                                                          //&[value, shift_amount_node],
                                                          //Some(addr));
            //value = new_value;
        //}

        //if info.width < (width as usize) {
            //let opcode = MOpcode::OpNarrow(info.width as WidthSpec);
            //let vtype = From::from(info.width);
            //let new_value = phiplacer.ssa
                                     //.verified_add_op(block, opcode, vtype, &[value], Some(addr));
            //value = new_value;
        //}
        //value
    //}
}
