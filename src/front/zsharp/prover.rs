use zokrates_pest_ast::*;
use crate::front::zsharp::ZGen;
use crate::front::zsharp::T;
use crate::front::zsharp::Ty;
use crate::front::zsharp::Value;
use crate::front::zsharp::const_bool;
use crate::front::zsharp::const_val;
use crate::front::zsharp::span_to_string;
use crate::front::zsharp::Op;
use std::collections::BTreeMap;
use crate::front::zsharp::blocks::*;
use crate::front::zsharp::*;
use crate::front::zsharp::pretty::pretty_name;
use log::warn;
use log::debug;
use std::cmp::Ordering;
use crate::ir::term::*;

use rug::Integer;

const STORE: usize = 0;
const LOAD: usize = 1;

const O_RET: &str = "%o000002";
const W_TS: &str = "%w1";
const W_AS: &str = "%w2";
const W_SP: &str = "%w4";
const W_BP: &str = "%w5";

#[derive(Debug, Clone)]
pub struct MemOp {
    // Address in usize for sorting
    pub addr: usize,
    // Address in T for witness generation
    pub addr_t: T,
    pub data_t: T,

    pub ls_t: Option<T>,
    // Timestamp in usize for sorting
    pub ts: Option<usize>,
    // Timestamp in T for witness generation
    pub ts_t: Option<T>,
}

impl MemOp {
    fn new_phy(addr: usize, addr_t: T, data_t: T) -> Self {
        let input = Self {
            addr,
            addr_t,
            data_t,
            ls_t: None,
            ts: None,
            ts_t: None,
        };
        input
    }

    fn new_vir(addr: usize, addr_t: T, data_t: T, ls_t: T, ts: usize, ts_t: T) -> Self {
        let input = Self {
            addr,
            addr_t,
            data_t,
            ls_t: Some(ls_t),
            ts: Some(ts),
            ts_t: Some(ts_t),
        };
        input
    }
}
// Ordering of MemOp solely by address
impl Ord for MemOp {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.addr, self.ts).cmp(&(other.addr, other.ts))
    }
}
impl PartialOrd for MemOp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for MemOp {
    fn eq(&self, other: &Self) -> bool {
        self.addr == other.addr && self.ts == other.ts
    }
}
impl Eq for MemOp {}

// We reserve indices for reg_in and reg_out to:
// reg  0   1   2   3   4   5   6   7   8   9
//      V  BN RET  TS  AS  RP  SP  BP  i8  i9
#[derive(Debug, Clone)]
pub struct ExecState {
    pub blk_id: usize,      // ID of the block
    pub reg_out: Vec<Option<T>>,    // Output register State
    pub succ_id: usize,     // ID of the successor block
    pub phy_mem_op: Vec<MemOp>,  // List of physical memory operations within the block
    pub vir_mem_op: Vec<MemOp>  // List of virtual memory operations within the block
}

impl ExecState {
    pub fn new(blk_id: usize, io_size: usize) -> Self {
        let input = Self {
            blk_id,
            reg_out: vec![None; io_size],
            succ_id: 0,
            phy_mem_op: Vec::new(),
            vir_mem_op: Vec::new(),
        };
        input
    }
}

impl<'ast> ZGen<'ast> {
    fn print_all_vars_in_scope(&self) {
        println!("\n\nVariables in scope:");

        let mut all_vars = BTreeMap::new();
        let binding = self.cvars_stack.borrow_mut();
        let maps = binding.last().unwrap();
        for map in maps {
            for (key, value) in map.iter() {
                all_vars.insert(key, value);
            }
        }
        
        for (key, value) in all_vars {
            print!("{} = ", pretty_name(key));
            value.pretty(&mut std::io::stdout().lock())
            .expect("error pretty-printing value");
            println!();
        }
    }

    fn t_to_usize(&self, a: T) -> Result<usize, String> {
        let t = const_val(a)?;
        match &t.term.op() {
            Op::Const(val) => {
                match val {
                    Value::Field(f) => {
                        let intg = f.i().to_usize().ok_or("Stack Overflow: array index exceeds usize limit.")?;
                        return Ok(intg);
                    }
                    Value::BitVector(bv) => {
                        let intg = bv.uint().to_usize().ok_or("Stack Overflow: array index exceeds usize limit.")?;
                        return Ok(intg);
                    }
                    Value::Int(i) => {
                        let intg = i.to_usize().ok_or("Stack Overflow: array index exceeds usize limit.")?;
                        return Ok(intg);
                    }
                    _ => {
                        return Err(format!("Fail to evaluate array index: index is not a number."));
                    }
                }
            }
            _ => { return Err(format!("This line should not be triggered unless const_val has been modified. Const_val needs to return Op::Const for Term.")) }
        }
    }

    // I am hacking cvars_stack to do the interpretation. Ideally we want a separate var_table to do so.
    // We only need BTreeMap<String, T> to finish evaluation, so the 2 Vecs of cvars_stack should always have
    // size 1, i.e. we use only one function and one scope.
    pub fn bl_eval_entry_fn<const VERBOSE: bool>(
        &self,
        entry_bl: usize,
        entry_regs: &Vec<Integer>, // Entry regs should match the input of the entry block
        bls: &Vec<Block<'ast>>,
        io_size: usize
    ) -> Result<(
        T, // Return value
        Vec<usize>, // Block ID
        Vec<Option<T>>, // Program input state
        Vec<ExecState>, // Block output states
        Vec<MemOp>, // Physical Memory operations
        Vec<MemOp> // Virtual Memory operations
    ), String> {
        if bls.len() < entry_bl {
            return Err(format!("Invalid entry_bl: entry_bl exceeds block size."));
        }

        // We assume that all errors has been handled in bl_gen functions        
        debug!("Block Eval Const entry: {}", entry_bl);

        // How many total blocks have we executed?
        let mut tr_size = 0;
        // bl_exec_count[i]: how many times have block i been executed?
        let mut bl_exec_count: Vec<usize> = vec![0; bls.len()];
        // bl_exec_state[i]: execution state of each block-execution
        let mut bl_exec_state: Vec<ExecState> = Vec::new();

        self.cvar_enter_function();
        let mut nb = entry_bl;
        let mut phy_mem: Vec<T> = Vec::new();
        let mut vir_mem: Vec<Option<T>> = Vec::new();
        let mut terminated = false;
        let mut phy_mem_op: Vec<MemOp>;
        let mut vir_mem_op: Vec<MemOp>;
        
        // Process input variables
        // Insert a 0 in front of the input variables for BN
        let entry_regs = &[vec![Integer::from(0)], entry_regs.clone()].concat();
        let mut prog_reg_in = vec![None; io_size];
        let mut i = 0;
        for (name, ty) in &bls[entry_bl].inputs {
            if let Some(x) = ty {
                assert!(i < entry_regs.len());

                let e = &(LiteralExpression::DecimalLiteral(
                    DecimalLiteralExpression {
                        value: DecimalNumber {
                            value: entry_regs[i].to_string(),
                            span: Span::new("", 0, 0).unwrap()
                        },
                        suffix: Some(match x {
                            Ty::Field => DecimalSuffix::Field(FieldSuffix {
                                span: Span::new("", 0, 0).unwrap()
                            }),
                            Ty::Uint(64) => DecimalSuffix::U64(U64Suffix {
                                span: Span::new("", 0, 0).unwrap()
                            }),
                            Ty::Uint(32) => DecimalSuffix::U32(U32Suffix {
                                span: Span::new("", 0, 0).unwrap()
                            }),
                            Ty::Uint(16) => DecimalSuffix::U16(U16Suffix {
                                span: Span::new("", 0, 0).unwrap()
                            }),
                            Ty::Uint(8) => DecimalSuffix::U8(U8Suffix {
                                span: Span::new("", 0, 0).unwrap()
                            }),
                            _ => panic!("Unsupported input type: {:?}!", x)
                        }),
                        span: Span::new("", 0, 0).unwrap()
                    }
                ));

                let val = self.literal_(e)?;
                self.declare_init_impl_::<true>(
                    name.to_string(),
                    x.clone(),
                    val,
                )?;
                i += 1;
            }
        }
        // Execute program
        while !terminated {
            bl_exec_count[nb] += 1;

            // Push-in new block state
            bl_exec_state.push(ExecState::new(nb, io_size));
            // If it is the first block, add input to prog_reg_in
            if tr_size == 0 {
                for i in 1..io_size {
                    prog_reg_in[i] = self.cvar_lookup(&format!("%i{:06}", i));
                }
            }
            // If not the first block, redefine output of the last block as input to this block
            // If an input is not defined in the previous output, then set it to 0 / false
            // Record the transition state
            else {
                for (name, ty) in &bls[nb].inputs {
                    if let Some(x) = ty {
                        let output_name = str::replace(name, "i", "o");
                        let val = self.cvar_lookup(&output_name).unwrap_or(
                            self.expr_impl_::<true>(&Expression::Literal(
                                match x {
                                    Ty::Bool => {
                                        LiteralExpression::BooleanLiteral(BooleanLiteralExpression {
                                            value: "false".to_string(),
                                            span: Span::new("", 0, 0).unwrap()
                                        })
                                    },
                                    _ => {
                                        LiteralExpression::DecimalLiteral(DecimalLiteralExpression {
                                            value: DecimalNumber {
                                                value: "0".to_string(),
                                                span: Span::new("", 0, 0).unwrap()
                                            },
                                            suffix: Some(match x {
                                                Ty::Field => DecimalSuffix::Field(FieldSuffix {
                                                    span: Span::new("", 0, 0).unwrap()
                                                }),
                                                Ty::Uint(64) => DecimalSuffix::U64(U64Suffix {
                                                    span: Span::new("", 0, 0).unwrap()
                                                }),
                                                Ty::Uint(32) => DecimalSuffix::U32(U32Suffix {
                                                    span: Span::new("", 0, 0).unwrap()
                                                }),
                                                Ty::Uint(16) => DecimalSuffix::U16(U16Suffix {
                                                    span: Span::new("", 0, 0).unwrap()
                                                }),
                                                Ty::Uint(8) => DecimalSuffix::U8(U8Suffix {
                                                    span: Span::new("", 0, 0).unwrap()
                                                }),
                                                _ => panic!("Unsupported input type: {:?}!", x)
                                            }),
                                            span: Span::new("", 0, 0).unwrap()
                                        })
                                    }
                                }
                            )).unwrap()
                        );
                        self.declare_init_impl_::<true>(
                            name.to_string(),
                            x.clone(),
                            val,
                        )?;
                    }
                }
                // Record the last transition state as the union of reg_in and reg_out
                for i in 1..io_size {
                    bl_exec_state[tr_size - 1].reg_out[i] = self.cvar_lookup(&format!("%o{:06}", i));
                    if bl_exec_state[tr_size - 1].reg_out[i].is_none() {
                        bl_exec_state[tr_size - 1].reg_out[i] = self.cvar_lookup(&format!("%i{:06}", i));
                    }
                }
            }

            if VERBOSE {
                self.print_all_vars_in_scope();
                print!("%PHY: [");
                for c in &phy_mem {
                    c.pretty(&mut std::io::stdout().lock())
                    .expect("error pretty-printcaring value");
                    print!(", ");
                }
                println!("]");
                print!("%VIR: [");
                for c in &vir_mem {
                    if let Some(c) = c { 
                        c.pretty(&mut std::io::stdout().lock())
                        .expect("error pretty-printing value");
                    } else {
                        print!("_");
                    }
                    print!(", ");
                }
                println!("]");
                let _ = &bls[nb].pretty();
                println!();
            }
            (nb, phy_mem, vir_mem, terminated, phy_mem_op, vir_mem_op) = self.bl_eval_impl_(&bls[nb], phy_mem, vir_mem)?;

            // Update successor block ID
            bl_exec_state[tr_size].succ_id = nb;
            // Update Memory Op
            bl_exec_state[tr_size].phy_mem_op = phy_mem_op;
            bl_exec_state[tr_size].vir_mem_op = vir_mem_op;
            tr_size += 1;
        }
        
        // Record the final transition state
        for i in 1..io_size {
            bl_exec_state[tr_size - 1].reg_out[i] = self.cvar_lookup(&format!("%o{:06}", i));
        }
        // Return value is just the value of the variable called "%RET"
        // Type of return value is checked during assignment
        let ret = self.cvar_lookup(O_RET).ok_or(format!(
            "Missing return value for one or more functions."
        ));

        let (phy_mem_list, vir_mem_list) = sort_by_mem(&bl_exec_state);
        Ok((ret?, bl_exec_count, prog_reg_in, bl_exec_state, phy_mem_list, vir_mem_list))
    }

    // Convert a usize into a Field value
    fn usize_to_field(&self, val: usize) -> Result<T, String> {
        let e = &(LiteralExpression::DecimalLiteral(
            DecimalLiteralExpression {
                value: DecimalNumber {
                    value: val.to_string(),
                    span: Span::new("", 0, 0).unwrap()
                },
                suffix: Some(DecimalSuffix::Field(FieldSuffix {
                        span: Span::new("", 0, 0).unwrap()
                    })),
                span: Span::new("", 0, 0).unwrap()
            }
        ));

        self.literal_(&e)
    }

    // Return type:
    // ret[0]: Index of next block,
    // ret[1]: Physical memory arrangement,
    // ret[2]: Virtual memory map,
    // ret[3]: Has the program terminated?
    // ret[4]: Pairs of [addr, data] for all physical (scoping) memory operations in the block
    // ret[5]: Quadruples of [addr, data, io, ts] for all virtual memory operations in the block
    fn bl_eval_impl_(
        &self, 
        bl: &Block<'ast>,
        mut phy_mem: Vec<T>,
        mut vir_mem: Vec<Option<T>>
    ) -> Result<(usize, Vec<T>, Vec<Option<T>>, bool, Vec<MemOp>, Vec<MemOp>), String> {
        let mut phy_mem_op: Vec<MemOp> = Vec::new();
        let mut vir_mem_op: Vec<MemOp> = Vec::new();

        (phy_mem, vir_mem, phy_mem_op, vir_mem_op) = self.bl_eval_inst_impl_(&bl.instructions, phy_mem, vir_mem, phy_mem_op, vir_mem_op)?;

        match &bl.terminator {
            BlockTerminator::Transition(e) => {
                match self.t_to_usize(self.expr_impl_::<true>(&e)?) {
                    Ok(nb) => { return Ok((nb, phy_mem, vir_mem, false, phy_mem_op, vir_mem_op)); }, 
                    _ => { return Err("Evaluation failed: block transition evaluated to an invalid block label".to_string()); }
                }
            }
            BlockTerminator::FuncCall(fc) => Err(format!("Evaluation failed: function call to {} needs to be converted to block label.", fc)),
            BlockTerminator::ProgTerm => Ok((0, phy_mem, vir_mem, true, phy_mem_op, vir_mem_op))
        }
    }

    fn bl_eval_inst_impl_(
        &self,
        inst: &Vec<BlockContent>,
        mut phy_mem: Vec<T>,
        mut vir_mem: Vec<Option<T>>,
        mut phy_mem_op: Vec<MemOp>,
        mut vir_mem_op: Vec<MemOp>,
    ) -> Result<(Vec<T>, Vec<Option<T>>, Vec<MemOp>, Vec<MemOp>), String> {
        for s in inst {
            match s {
                BlockContent::MemPush((var, _, offset)) => {
                    let sp_t = self.cvar_lookup(W_SP).ok_or(format!("Push to %PHY failed: %SP is uninitialized."))?;
                    let sp = self.t_to_usize(sp_t)?;
                    if sp + offset != phy_mem.len() {
                        return Err(format!("Error processing %PHY push: index {sp} + {offset} does not match with stack size."));
                    } else {
                        let e = self.cvar_lookup(&var).ok_or(format!("Push to %PHY failed: pushing an out-of-scope variable: {}.", var))?;
                        phy_mem.push(e);
                    }
                    // Convert val_t to field for MemOp
                    let mut val_t = self.cvar_lookup(&var).unwrap();
                    if val_t.type_() != &Ty::Field {
                        val_t = uint_to_field(val_t).unwrap();
                    }
                    phy_mem_op.push(MemOp::new_phy(sp + offset, self.usize_to_field(sp + offset)?, val_t));
                }
                BlockContent::MemPop((var, _, offset)) => {
                    let bp_t = self.cvar_lookup(W_BP).ok_or(format!("Pop from %PHY failed: %BP is uninitialized."))?;
                    let bp = self.t_to_usize(bp_t)?;
                    if bp + offset >= phy_mem.len() {
                        return Err(format!("Error processing %PHY pop: index out of bound."));
                    } else {
                        let t = phy_mem[bp + offset].clone();
                        self.cvar_assign(&var, t)?;
                    }
                    // Convert val_t to field for MemOp
                    let mut val_t = self.cvar_lookup(&var).unwrap();
                    if val_t.type_() != &Ty::Field {
                        val_t = uint_to_field(val_t).unwrap();
                    }
                    phy_mem_op.push(MemOp::new_phy(bp + offset, self.usize_to_field(bp + offset)?, val_t));         
                }
                BlockContent::ArrayInit((arr, _, len_expr)) => {
                    // Declare the array as a pointer (field), set to %AS
                    let as_t = self.cvar_lookup(W_AS).ok_or(format!("Array initialization failed: %AS is uninitialized."))?;
                    self.declare_init_impl_::<true>(
                        arr.to_string(),
                        Ty::Field,
                        as_t.clone(),
                    )?;
                    // Increment %AS by size of array
                    let mut len_t = self.expr_impl_::<true>(&len_expr).unwrap();
                    if len_t.type_() != &Ty::Field {
                        len_t = uint_to_field(len_t).unwrap();
                    }
                    let new_as_t = add(as_t, len_t).unwrap();
                    self.cvar_assign(W_AS, new_as_t)?;
                }
                BlockContent::Store((val_expr, _, arr, id_expr, init)) => {
                    let mut val_t = self.expr_impl_::<true>(&val_expr)?;
                    let mut id_t = self.expr_impl_::<true>(&id_expr)?;

                    // Add array offset to obtain address
                    let offset_t = self.cvar_lookup(arr).ok_or(format!("Store failed: array {} is uninitialized.", arr))?;
                    if id_t.type_() != &Ty::Field {
                        id_t = uint_to_field(id_t).unwrap();
                    }
                    let addr_t = add(id_t, offset_t).unwrap();
                    let addr = self.t_to_usize(addr_t.clone())?;
                    // update vir_mem, pad if necessary
                    if addr >= vir_mem.len() { vir_mem.extend(vec![None; addr + 1 - vir_mem.len()]); }
                    vir_mem[addr] = Some(val_t.clone());

                    // Update vir_mem_op
                    let ls_t = self.expr_impl_::<false>(&Expression::Literal(LiteralExpression::DecimalLiteral(DecimalLiteralExpression {
                        value: DecimalNumber {
                            value: STORE.to_string(),
                            span: Span::new("", 0, 0).unwrap()
                        },
                        suffix: Some(DecimalSuffix::Field(FieldSuffix {
                            span: Span::new("", 0, 0).unwrap()
                        })),
                        span: Span::new("", 0, 0).unwrap()
                    }))).unwrap();

                    // %TS = %TS + 1
                    if !init {
                        self.bl_eval_stmt_impl_(&bl_gen_increment_stmt(W_TS, 1, &Ty::Field)).unwrap();
                    }
                    let ts_t = self.cvar_lookup(W_TS).ok_or(format!("STORE failed: %TS is uninitialized."))?;
                    let ts = self.t_to_usize(ts_t.clone())?;

                    // Convert val_t to field for MemOp
                    if val_t.type_() != &Ty::Field {
                        val_t = uint_to_field(val_t).unwrap();
                    }
                    vir_mem_op.push(MemOp::new_vir(
                        addr,
                        addr_t,
                        val_t,
                        ls_t,
                        ts,
                        ts_t
                    ));
                }
                BlockContent::Load((var, ty, arr, id_expr)) => {
                    let mut id_t = self.expr_impl_::<true>(&id_expr)?;

                    // Add array offset to obtain address
                    let offset_t = self.cvar_lookup(arr).ok_or(format!("Store failed: array {} is uninitialized.", arr))?;
                    if id_t.type_() != &Ty::Field {
                        id_t = uint_to_field(id_t).unwrap();
                    }
                    let addr_t = add(id_t, offset_t).unwrap();
                    let addr = self.t_to_usize(addr_t.clone())?;

                    // Declare the variable
                    let mut val_t = vir_mem[addr].clone().ok_or(format!("LOAD failed: entry {} is uninitialized.", addr))?;
                    let entry_ty = val_t.type_();
                    if ty != entry_ty {
                        return Err(format!(
                            "Assignment type mismatch: {} annotated vs {} actual",
                            ty, entry_ty,
                        ));
                    }
                    self.cvar_declare_init(var.clone(), ty, val_t.clone())?;

                    // Update vir_mem_op
                    let ls_t = self.expr_impl_::<false>(&Expression::Literal(LiteralExpression::DecimalLiteral(DecimalLiteralExpression {
                        value: DecimalNumber {
                            value: LOAD.to_string(),
                            span: Span::new("", 0, 0).unwrap()
                        },
                        suffix: Some(DecimalSuffix::Field(FieldSuffix {
                            span: Span::new("", 0, 0).unwrap()
                        })),
                        span: Span::new("", 0, 0).unwrap()
                    }))).unwrap();
                    let ts_t = self.cvar_lookup(W_TS).ok_or(format!("STORE failed: %TS is uninitialized."))?;
                    let ts = self.t_to_usize(ts_t.clone())?;

                    // Convert val_t to field for MemOp
                    if val_t.type_() != &Ty::Field {
                        val_t = uint_to_field(val_t).unwrap();
                    }
                    vir_mem_op.push(MemOp::new_vir(
                        addr,
                        addr_t,
                        val_t,
                        ls_t,
                        ts,
                        ts_t
                    ));
                }
                BlockContent::DummyLoad() => {
                    // Addr is 0
                    let addr_t = self.expr_impl_::<false>(&Expression::Literal(LiteralExpression::DecimalLiteral(DecimalLiteralExpression {
                        value: DecimalNumber {
                            value: 0.to_string(),
                            span: Span::new("", 0, 0).unwrap()
                        },
                        suffix: Some(DecimalSuffix::Field(FieldSuffix {
                            span: Span::new("", 0, 0).unwrap()
                        })),
                        span: Span::new("", 0, 0).unwrap()
                    }))).unwrap();
                    let addr = self.t_to_usize(addr_t.clone())?;

                    // Val is vir_mem[0]
                    let mut val_t = vir_mem[addr].clone().ok_or(format!("LOAD failed: entry {} is uninitialized.", addr))?;

                    // Update vir_mem_op
                    let ls_t = self.expr_impl_::<false>(&Expression::Literal(LiteralExpression::DecimalLiteral(DecimalLiteralExpression {
                        value: DecimalNumber {
                            value: LOAD.to_string(),
                            span: Span::new("", 0, 0).unwrap()
                        },
                        suffix: Some(DecimalSuffix::Field(FieldSuffix {
                            span: Span::new("", 0, 0).unwrap()
                        })),
                        span: Span::new("", 0, 0).unwrap()
                    }))).unwrap();
                    let ts_t = self.cvar_lookup(W_TS).ok_or(format!("STORE failed: %TS is uninitialized."))?;
                    let ts = self.t_to_usize(ts_t.clone())?;

                    // Convert val_t to field for MemOp
                    if val_t.type_() != &Ty::Field {
                        val_t = uint_to_field(val_t).unwrap();
                    }
                    vir_mem_op.push(MemOp::new_vir(
                        addr,
                        addr_t,
                        val_t,
                        ls_t,
                        ts,
                        ts_t
                    ));
                }
                BlockContent::Branch((cond, if_inst, else_inst)) => {
                    match self.expr_impl_::<true>(&cond).and_then(|v| {
                        const_bool(v)
                            .ok_or_else(|| "interpreting expr as const bool failed".to_string())
                    }) {
                        Ok(true) => {
                            (phy_mem, vir_mem, phy_mem_op, vir_mem_op) = self.bl_eval_inst_impl_(if_inst, phy_mem, vir_mem, phy_mem_op, vir_mem_op)?;
                        },
                        Ok(false) => {
                            (phy_mem, vir_mem, phy_mem_op, vir_mem_op) = self.bl_eval_inst_impl_(else_inst, phy_mem, vir_mem, phy_mem_op, vir_mem_op)?;
                        },
                        Err(err) => return Err(format!(
                            "Const conditional expression eval failed: {} at\n{}",
                            err,
                            span_to_string(cond.span()),
                        ))
                    }
                }
                BlockContent::Stmt(s) => {
                    self.bl_eval_stmt_impl_(s)?;
                }
            }
        };
        Ok((phy_mem, vir_mem, phy_mem_op, vir_mem_op))
    }

    fn bl_eval_stmt_impl_(
        &self,
        s: &Statement,
    ) -> Result<(), String> {
        match s {
            Statement::Return(_) => {
                return Err(format!("Blocks should not contain return statements."));
            }
            // %PHY should never appear in an assertion statement
            Statement::Assertion(a) => {
                match self.expr_impl_::<true>(&a.expression).and_then(|v| {
                    const_bool(v)
                        .ok_or_else(|| "interpreting expr as const bool failed".to_string())
                }) {
                    Ok(true) => {},
                    Ok(false) => return Err(format!(
                        "Const assert failed: {} at\n{}",
                        a.message
                            .as_ref()
                            .map(|m| m.value.as_ref())
                            .unwrap_or("(no error message given)"),
                        span_to_string(a.expression.span()),
                    )),
                    Err(err) => return Err(format!(
                        "Const assert expression eval failed: {} at\n{}",
                        err,
                        span_to_string(a.expression.span()),
                    ))
                }
            }
            Statement::Iteration(_) => {
                return Err(format!("Blocks should not contain iteration statements."));
            }
            Statement::WhileLoop(_) => {
                return Err(format!("Blocks should not contain while loop statements."));
            }
            Statement::Conditional(_c) => {
                panic!("Blocks should not contain conditional statements.")
                /*
                match self.expr_impl_::<true>(&c.condition).and_then(|v| {
                    const_bool(v)
                        .ok_or_else(|| "interpreting expr as const bool failed".to_string())
                }) {
                    Ok(true) => {
                        for s in &c.ifbranch {
                            self.bl_eval_stmt_impl_(s)?;
                        }
                    },
                    Ok(false) => {
                        for s in &c.elsebranch {
                            self.bl_eval_stmt_impl_(s)?;
                        }
                    },
                    Err(err) => return Err(format!(
                        "Const conditional expression eval failed: {} at\n{}",
                        err,
                        span_to_string(c.condition.span()),
                    ))
                }
                */
            }
            Statement::Definition(d) => {
                // XXX(unimpl) multi-assignment unimplemented
                assert!(d.lhs.len() <= 1);

                self.set_lhs_ty_defn::<true>(&d)?;
                let e = self.expr_impl_::<true>(&d.expression)?;

                if let Some(l) = d.lhs.first() {
                    match l {
                        TypedIdentifierOrAssignee::Assignee(l) => {
                            let strict = match &d.expression {
                                Expression::Unary(u) => {
                                    matches!(&u.op, UnaryOperator::Strict(_))
                                }
                                _ => false,
                            };
                            self.assign_impl_::<true>(&l.id.value, &l.accesses[..], e, strict)?;
                        }
                        TypedIdentifierOrAssignee::TypedIdentifier(l) => {
                            let decl_ty = self.type_impl_::<true>(&l.ty)?;
                            let ty = e.type_();
                            if &decl_ty != ty {
                                return Err(format!(
                                    "Assignment type mismatch: {} annotated vs {} actual",
                                    decl_ty, ty,
                                ));
                            }
                            self.declare_init_impl_::<true>(
                                l.identifier.value.clone(),
                                decl_ty,
                                e,
                            )?;
                        }
                    }
                } else {
                    warn!("Statement with no LHS!");
                }
            }
            Statement::CondStore(_) => { panic!("Blocks should not contain conditional store statements.") }
            Statement::Witness(_) => { panic!("Witness statements unsupported.") }
            Statement::ArrayDecl(_) => { panic!("Blocks should not contain array declaration statements.") }
        };
        Ok(())
    }
}

pub fn sort_by_mem(bl_exec_state: &Vec<ExecState>) -> (Vec<MemOp>, Vec<MemOp>) {
    let mut sorted_phy_mem_op_list = Vec::new();
    let mut sorted_vir_mem_op_list = Vec::new();
    for b in bl_exec_state {
        sorted_phy_mem_op_list.append(&mut b.phy_mem_op.clone());
        sorted_vir_mem_op_list.append(&mut b.vir_mem_op.clone());
    }
    sorted_phy_mem_op_list.sort();
    sorted_vir_mem_op_list.sort();
    (sorted_phy_mem_op_list, sorted_vir_mem_op_list)
}