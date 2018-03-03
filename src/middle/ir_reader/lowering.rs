//! (see [`lower_simpleast`](lower_simpleast))

use super::simple_ast as sast;
use middle::ir;
use middle::ir::MOpcode as IrOpcode;
use middle::ssa::cfg_traits::{CFGMod, CFG};
use middle::ssa::ssa_traits::{SSAExtra, SSAMod, ValueInfo, ValueType, SSA};
use middle::ssa::ssastorage::{NodeData, SSAStorage};
use std::collections::HashMap;
use std::error;
use std::fmt;
use std::mem;

pub type Result<T> = ::std::result::Result<T, LoweringError>;

/// Lowers [AST](sast) into the provided [`SSAStorage`](SSAStorage)
pub fn lower_simpleast<'a>(ssa: &'a mut SSAStorage, sfn: sast::Function) -> Result<()> {
    LowerSsa::new(ssa).lower_function(sfn)
}

#[derive(Debug)]
pub enum LoweringError {
    /// If an operation on the [`SSAStorage`](SSAStorage) fails
    SsaError,
    /// If the AST was invalid somehow
    InvalidAst(String),
}

type SSABlock = <SSAStorage as CFG>::ActionRef;
type SSAValue = <SSAStorage as SSA>::ValueRef;

/// from [`ssaconstructor`](::frontend::ssaconstructor)
const FALSE_EDGE: u8 = 0;
const TRUE_EDGE: u8 = 1;
const UNCOND_EDGE: u8 = 2;

struct LowerSsa<'a> {
    ssa: &'a mut SSAStorage,
    blocks: HashMap<ir::MAddress, SSABlock>,
    values: HashMap<sast::ValueRef, SSAValue>,
    regnames: Vec<sast::PhysReg>,
}

impl<'a> LowerSsa<'a> {
    fn new(ssa: &'a mut SSAStorage) -> Self {
        LowerSsa {
            ssa,
            blocks: HashMap::new(),
            values: HashMap::new(),
            regnames: Vec::new(),
        }
    }

    fn lower_function(mut self, sfn: sast::Function) -> Result<()> {
        self.regnames = sfn.register_list;
        if self.regnames.len() >= u8::max_value() as usize {
            return Err(LoweringError::InvalidAst(format!(
                "too many registers: {}",
                self.regnames.len()
            )));
        }

        let exit_node = self.ssa.insert_dynamic()?;
        self.ssa.set_exit_node(exit_node);

        let entry_node = self.ssa.insert_block(ir::MAddress::new(0, 0))?;
        self.ssa.set_entry_node(entry_node);

        // when we're lowering a block, we need to know what block comes afterwards;
        // so when we iterate over the blocks, we lower the *previous* block we saw,
        // so then that block's next block is just the current block
        let mut opt_prev_sbb: Option<sast::BasicBlock> = None;
        for sbb in sfn.basic_blocks {
            let sbb_addr = sbb.addr;
            if let Some(prev_sbb) = mem::replace(&mut opt_prev_sbb, Some(sbb)) {
                // lower `prev`; `sbb` is `prev`'s next
                self.lower_basicblock(prev_sbb, Some(sbb_addr))?;
            } else {
                // `sbb` is the first block
                let bb = self.block_at(sbb_addr)?;
                self.ssa.insert_control_edge(entry_node, bb, UNCOND_EDGE);
            }
        }
        if let Some(last_sbb) = opt_prev_sbb {
            // lower the last block
            self.lower_basicblock(last_sbb, None)?;
        } else {
            // there were 0 blocks
            // I think this is a sane thing to do in this case :P
            self.ssa
                .insert_control_edge(entry_node, exit_node, UNCOND_EDGE);
        }

        let final_state = self.ssa.registers_in(exit_node)?;
        for (sreg, sop) in sfn.final_reg_state {
            // a bit of a hack...
            let reg_idx = if sreg.0 != "mem" {
                self.index_of_reg(&sreg)?
            } else {
                self.regnames.len() as u8
            };
            let op = self.lower_operand(sop)?;
            self.ssa.op_use(final_state, reg_idx, op);
        }

        self.ssa
            .map_registers(self.regnames.into_iter().map(|x| x.0).collect());

        Ok(())
    }

    fn lower_basicblock(
        &mut self,
        sbb: sast::BasicBlock,
        opt_next_addr: Option<ir::MAddress>,
    ) -> Result<()> {
        let bb = self.block_at(sbb.addr)?;

        for sop in sbb.ops {
            let (res, opt_op_addr) = self.lower_operation(sop)?;
            let op_addr = opt_op_addr.unwrap_or(sbb.addr);
            self.ssa.insert_into_block(res, bb, op_addr);
        }

        // can't use `map_or_else` because either operation may fail
        let next_node = if let Some(next_addr) = opt_next_addr {
            self.block_at(next_addr)?
        } else {
            self.ssa.exit_node()?
        };
        match sbb.jump {
            Some(sast::Jump::Uncond(tgt)) => {
                let tgt_bb = self.block_at(tgt)?;
                self.ssa.insert_control_edge(bb, tgt_bb, UNCOND_EDGE);
            }
            Some(sast::Jump::Cond(sel_sop, if_tgt, opt_else_tgt)) => {
                let sel_op = self.lower_operand(sel_sop)?;
                let if_bb = self.block_at(if_tgt)?;
                let else_bb = opt_else_tgt.map_or(Ok(next_node), |a| self.block_at(a))?;
                self.ssa.set_selector(sel_op, bb);
                self.ssa.insert_control_edge(bb, if_bb, TRUE_EDGE);
                self.ssa.insert_control_edge(bb, else_bb, FALSE_EDGE);
            }
            None => {
                // fallthrough to `next`
                self.ssa.insert_control_edge(bb, next_node, UNCOND_EDGE);
            }
        }

        Ok(())
    }

    fn lower_operation(
        &mut self,
        sopn: sast::Operation,
    ) -> Result<(SSAValue, Option<ir::MAddress>)> {
        Ok(match sopn {
            sast::Operation::Phi(vr, ty, sops) => {
                let vi = lower_valueinfo(ty);
                let res = self.ssa.insert_phi(vi)?;
                for sop in sops.into_iter().rev() {
                    let op = self.lower_operand(sop)?;
                    self.ssa.phi_use(res, op);
                }
                self.values.insert(vr, res);
                (res, None)
            }

            sast::Operation::Assign(opt_addr, vr, ty, sexpr) => {
                let vi = lower_valueinfo(ty);
                let (opcode, sops) = match sexpr {
                    sast::Expr::Infix(sop0, sopcode, sop1) => {
                        (lower_infix_op(sopcode), vec![sop0, sop1])
                    }
                    sast::Expr::Prefix(sopcode, sop0) => (lower_prefix_op(sopcode), vec![sop0]),
                    sast::Expr::Load(sop0, sop1) => (IrOpcode::OpLoad, vec![sop0, sop1]),
                    sast::Expr::Store(sop0, sop1, sop2) => {
                        (IrOpcode::OpStore, vec![sop0, sop1, sop2])
                    }
                    sast::Expr::Resize(rst, ws, sop0) => (lower_resize_op(rst, ws), vec![sop0]),
                };
                let res = self.ssa.insert_op(opcode, vi, None)?;
                for (i, sop) in sops.into_iter().enumerate() {
                    let op = self.lower_operand(sop)?;
                    self.ssa.op_use(res, i as u8, op);
                }
                self.values.insert(vr, res);
                (res, opt_addr)
            }

            sast::Operation::Call(opt_addr, tgt, sargs) => {
                // TODO: round-trip call `ValueInfo`
                let vi = ValueInfo::new_unresolved(ir::WidthSpec::Unknown);
                let res = self.ssa.insert_op(IrOpcode::OpCall, vi, None)?;
                let tgt_op = self.lower_operand(tgt)?;
                self.ssa.op_use(res, 0, tgt_op);
                for sarg in sargs {
                    let reg_idx = self.index_of_reg(&sarg.formal)?;
                    let op = self.lower_operand(sarg.actual)?;
                    self.ssa.op_use(res, reg_idx + 1, op);
                }
                (res, opt_addr)
            }
        })
    }

    fn lower_operand(&mut self, sop: sast::Operand) -> Result<SSAValue> {
        match sop {
            sast::Operand::Comment(s) => {
                // TODO: round-trip comment `ValueInfo`
                let vi = ValueInfo::new_unresolved(ir::WidthSpec::Unknown);
                Ok(self.ssa.insert_comment(vi, s)?)
            }
            sast::Operand::ValueRef(r) => {
                if let Some(x) = self.values.get(&r).cloned() {
                    Ok(x)
                } else {
                    Err(LoweringError::InvalidAst(format!(
                        "no value reference: %{}",
                        r.0
                    )))
                }
            }
            sast::Operand::Const(v) => Ok(self.ssa.insert_const(v)?),
        }
    }

    fn block_at(&mut self, at: ir::MAddress) -> Result<SSABlock> {
        use std::collections::hash_map::Entry;
        // can't use `or_insert_with` because `ssa.insert_block` may fail
        Ok(*match self.blocks.entry(at) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(self.ssa.insert_block(at)?),
        })
    }

    fn index_of_reg(&self, sreg: &sast::PhysReg) -> Result<u8> {
        // TODO: more efficient as a HashMap<Reg, u8>?
        if let Some(x) = self.regnames.iter().position(|r| r == sreg) {
            // we already checked that `regnames.len() < u8::max_value()`
            Ok(x as u8)
        } else {
            Err(LoweringError::InvalidAst(format!(
                "no physical register: {}",
                sreg.0
            )))
        }
    }
}

fn lower_infix_op(siop: sast::InfixOp) -> IrOpcode {
    match siop {
        sast::InfixOp::Add => IrOpcode::OpAdd,
        sast::InfixOp::Sub => IrOpcode::OpSub,
        sast::InfixOp::Mul => IrOpcode::OpMul,
        sast::InfixOp::Div => IrOpcode::OpDiv,
        sast::InfixOp::Mod => IrOpcode::OpMod,
        sast::InfixOp::And => IrOpcode::OpAnd,
        sast::InfixOp::Or => IrOpcode::OpOr,
        sast::InfixOp::Xor => IrOpcode::OpXor,
        sast::InfixOp::Eq => IrOpcode::OpEq,
        sast::InfixOp::Gt => IrOpcode::OpGt,
        sast::InfixOp::Lt => IrOpcode::OpLt,
        sast::InfixOp::Lsl => IrOpcode::OpLsl,
        sast::InfixOp::Lsr => IrOpcode::OpLsr,
    }
}

fn lower_prefix_op(spop: sast::PrefixOp) -> IrOpcode {
    match spop {
        sast::PrefixOp::Not => IrOpcode::OpNot,
    }
}

fn lower_resize_op(srst: sast::ResizeType, sws: sast::WidthSpec) -> IrOpcode {
    match srst {
        sast::ResizeType::Narrow => IrOpcode::OpNarrow(sws.0),
        sast::ResizeType::SignExt => IrOpcode::OpSignExt(sws.0),
        sast::ResizeType::ZeroExt => IrOpcode::OpZeroExt(sws.0),
    }
}

fn lower_valueinfo(sty: sast::Type) -> ValueInfo {
    let ws = ir::WidthSpec::Known((sty.0).0);
    match sty.1 {
        sast::RefSpec::Scalar => ValueInfo::new_scalar(ws),
        sast::RefSpec::Reference => ValueInfo::new_reference(ws),
        sast::RefSpec::Unknown => ValueInfo::new_unresolved(ws),
    }
}

/// [`SSAStorage`][SSAStorage] methods return `Option`,
/// so we convert `None`s into [`SsaError`][LoweringError::SsaError]
impl From<::std::option::NoneError> for LoweringError {
    fn from(_: ::std::option::NoneError) -> Self {
        LoweringError::SsaError
    }
}

impl error::Error for LoweringError {
    fn description(&self) -> &str {
        match *self {
            LoweringError::SsaError => "could not perform an `SSAStorage` operation",
            LoweringError::InvalidAst(_) => "invalid ast",
        }
    }
}

impl fmt::Display for LoweringError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            LoweringError::SsaError => write!(f, "could not perform an `SSAStorage` operation"),
            LoweringError::InvalidAst(ref s) => write!(f, "invalid ast: {}", s),
        }
    }
}
