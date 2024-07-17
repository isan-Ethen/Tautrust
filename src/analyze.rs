// rustc crates
use rustc_ast::ast::LitKind;
use rustc_hir::Lit;
use rustc_middle::mir::{BinOp, UnOp};
use rustc_middle::thir::LocalVarId;
use rustc_middle::thir::LogicalOp;
use rustc_middle::ty::TyCtxt;
use rustc_middle::ty::{Ty, TyKind};
use rustc_span::{
    def_id::{DefId, LocalDefId},
    Span,
};

// std crates
use std::collections::{HashMap as Map, VecDeque};
use std::io::Write;
use std::process::Command;
use std::rc::Rc;

// Own crates
use crate::thir::rthir::*;
mod lir;
use lir::*;

pub fn analyze<'tcx>(
    main_id: LocalDefId, fn_map: Map<LocalDefId, Rc<RThir<'tcx>>>, tcx: TyCtxt<'tcx>,
) -> Result<(), AnalysisError> {
    Analyzer::run(main_id, fn_map, tcx)
}

struct Analyzer<'tcx> {
    fn_map: Map<LocalDefId, Rc<RThir<'tcx>>>,
    tcx: TyCtxt<'tcx>,
    context_stack: Vec<Context<'tcx>>,
    current_context: Context<'tcx>,
}

#[derive(Clone)]
struct Context<'tcx> {
    name: String,
    path: VecDeque<Lir<'tcx>>,
    var_map: Map<LocalVarId, (String, Ty<'tcx>)>,
}

impl<'tcx> Context<'tcx> {
    fn new() -> Self {
        Self { name: "main".to_string(), path: VecDeque::new(), var_map: Map::new() }
    }

    fn from(
        name: String, // condition: Option<String>,
        path: VecDeque<Lir<'tcx>>,
        var_map: Map<LocalVarId, (String, Ty<'tcx>)>,
    ) -> Self {
        Self { name, path, var_map }
    }

    fn name(&self) -> String { self.name.clone() }

    fn len(&self) -> usize { self.path.len() }

    fn get_assumptions_for_verify(&self) -> Result<String, AnalysisError> {
        let mut smt = String::new();
        let len = self.len();
        let mut cnt = 0;
        loop {
            if len - 1 == cnt {
                smt.push_str(&self.get_assumption(cnt).to_assert());
                break;
            }
            smt.push_str(&self.get_assumption(cnt).to_smt()?);
            cnt += 1;
        }
        Ok(smt)
    }

    fn get_assumptions(&self) -> Result<String, AnalysisError> {
        let mut smt = String::new();
        for assumption in self.path.iter() {
            smt.push_str(&assumption.to_smt()?);
        }
        Ok(smt)
    }

    fn get_assumption(&self, index: usize) -> &Lir<'tcx> { &self.path[index] }

    fn get_latest_span(&self) -> Span { self.path.back().expect("Lir not found").get_span() }

    fn add_assumption(&mut self, lir: Lir<'tcx>) { self.path.push_back(lir); }

    fn insert_var(&mut self, var_id: &LocalVarId, name: String, ty: &Ty<'tcx>) {
        self.var_map.insert(var_id.clone(), (name, ty.clone()));
    }

    fn get_var(&self, var_id: &LocalVarId) -> (String, Ty<'tcx>) {
        self.var_map.get(var_id).unwrap().clone()
    }
}

enum AnalysisType {
    Return(Option<String>),
    Other,
}

impl AnalysisType {
    fn is_return(&self) -> bool {
        match self {
            AnalysisType::Return(..) => true,
            AnalysisType::Other => false,
        }
    }
}

#[derive(Debug)]
pub enum AnalysisError {
    FunctionNotFound(LocalDefId),
    UnsupportedPattern(String),
    RandFunctions,
    VerifyError { message: String, span: Span },
}

impl<'tcx> Analyzer<'tcx> {
    /// Constructors
    /// - new
    /// - run

    pub fn new(fn_map: Map<LocalDefId, Rc<RThir<'tcx>>>, tcx: TyCtxt<'tcx>) -> Self {
        Self {
            fn_map,
            tcx,
            // path_map: Map::new(),
            context_stack: Vec::new(),
            current_context: Context::new(),
        }
    }

    pub fn run(
        main_id: LocalDefId, fn_map: Map<LocalDefId, Rc<RThir<'tcx>>>, tcx: TyCtxt<'tcx>,
    ) -> Result<(), AnalysisError> {
        let mut analyzer = Analyzer::new(fn_map, tcx);
        let main = analyzer.get_fn(main_id)?;
        analyzer.analyze_local_fn(main, &[])
    }

    /// Utility functions
    /// - verify
    /// - get_current_assumptions_for_verify
    /// - get_current_assumptions
    /// - get_current_span
    /// - get_fn
    /// - add_assumption
    /// - add_parameter
    /// - get_var
    /// - get_local_fn
    /// - get_fn_info
    /// - assign_new_value

    fn verify(&self) -> Result<(), AnalysisError> {
        let mut child = Command::new("z3")
            .args(["-in", "-model"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("Run z3 failed");

        let mut stdin = child.stdin.take().expect("Open std failed");
        let mut smt = self.get_current_assumptions_for_verify()?;
        smt += "(check-sat)";
        println!("{}", smt);
        stdin.write_all(smt.as_bytes()).expect("Write smt failed");
        drop(stdin);

        let output = child.wait_with_output().expect("Get stdout failed");
        let result = String::from_utf8(output.stdout).expect("Load result failed");
        if &result != "unsat\n" {
            return Err(AnalysisError::VerifyError {
                message: result,
                span: self.get_current_span(),
            });
        }

        println!("Verification success!\n");

        Ok(())
    }

    fn get_current_assumptions_for_verify(&self) -> Result<String, AnalysisError> {
        let smt = self.current_context.get_assumptions_for_verify()?;
        Ok(smt)
    }

    fn get_current_assumptions(&self) -> Result<String, AnalysisError> {
        let smt = self.current_context.get_assumptions()?;
        Ok(smt)
    }

    fn get_current_span(&self) -> Span { self.current_context.get_latest_span() }

    fn get_fn(&self, fn_id: LocalDefId) -> Result<Rc<RThir<'tcx>>, AnalysisError> {
        self.fn_map.get(&fn_id).cloned().ok_or(AnalysisError::FunctionNotFound(fn_id))
    }

    fn get_local_fn(&self, def_id: &DefId) -> Option<Rc<RThir<'tcx>>> {
        if def_id.is_local() {
            Some(self.fn_map.get(&def_id.expect_local()).expect("Get local fn failed").clone())
        } else {
            None
        }
    }

    fn get_fn_info(&self, def_id: &DefId) -> Vec<String> {
        let def_path = self.tcx.def_path_str(*def_id);
        def_path
            .split(|c| c == ':' || c == '"' || c == '\\')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    }

    fn get_var(&self, var_id: LocalVarId) -> String { self.current_context.get_var(&var_id).0 }

    fn add_assumption(&mut self, constraint: String, expr: Rc<RExpr<'tcx>>) {
        let new_assumption = Lir::new_assume(constraint, expr);
        self.current_context.add_assumption(new_assumption);
    }

    fn add_parameter(
        &mut self, name: String, ty: &Ty<'tcx>, var_id: &LocalVarId, pat: Rc<RExpr<'tcx>>,
    ) {
        let parameter = Lir::new_parameter(name.to_string(), ty.clone(), pat);
        self.current_context.add_assumption(parameter);
        self.current_context.insert_var(var_id, name, ty);
    }

    fn assign_new_value(
        &mut self, expr: Rc<RExpr<'tcx>>,
    ) -> Result<(String, String), AnalysisError> {
        match expr.kind {
            RExprKind::VarRef { id } => {
                let (current_symbol, ty) = self.current_context.get_var(&id);
                let new_symbol = if current_symbol.starts_with(self.current_context.name().as_str())
                {
                    format!("{}+", current_symbol)
                } else {
                    format!("{}_{}", self.current_context.name(), current_symbol)
                };
                let new_parameter =
                    Lir::new_parameter(new_symbol.clone(), ty.clone(), expr.clone());
                self.current_context.add_assumption(new_parameter);
                self.current_context.insert_var(&id, new_symbol.clone(), &ty);
                Ok((new_symbol, current_symbol))
            }
            _ => unreachable!(),
        }
    }

    /// Main analyze functions
    /// - analyze_local_fn
    /// - analyze_params
    /// - analyze_body
    /// - analyze_expr

    fn analyze_local_fn(
        &mut self, rthir: Rc<RThir<'tcx>>, args: &[Rc<RExpr<'tcx>>],
    ) -> Result<(), AnalysisError> {
        self.analyze_params(&rthir.params, args)?;
        if let Some(body) = &rthir.body {
            self.analyze_body((*body).clone())?;
        }
        Ok(())
    }

    fn analyze_params(
        &mut self, params: &[RParam<'tcx>], args: &[Rc<RExpr<'tcx>>],
    ) -> Result<(), AnalysisError> {
        use RExprKind::*;
        use RPatKind::*;

        for (param, arg) in params.iter().zip(args.iter()) {
            if let Some(pat) = &param.pat {
                if let RExpr { kind: Pat { kind }, .. } = &**pat {
                    match kind {
                        Binding { name, ty, var, .. } => {
                            let context_name = format!("{}_{}", self.current_context.name(), name);
                            self.add_parameter(context_name.clone(), ty, var, pat.clone());
                            let value = self.expr_to_constraint(arg.clone())?;
                            self.add_assumption(
                                format!("(= {} {})", context_name, value),
                                arg.clone(),
                            );
                        }
                        Wild => (),
                        _ => return Err(AnalysisError::UnsupportedPattern(format!("{:?}", kind))),
                    }
                }
            }
        }
        Ok(())
    }

    fn analyze_body(&mut self, body: Rc<RExpr<'tcx>>) -> Result<(), AnalysisError> {
        if let RExpr { kind: RExprKind::Block { stmts, expr: _ }, .. } = &*body {
            for stmt in stmts {
                if self.analyze_expr(stmt.clone())?.is_return() {
                    break;
                }
            }
        } else {
            return Err(AnalysisError::UnsupportedPattern("Unknown body pattern".into()));
        }
        Ok(())
    }

    fn analyze_expr(&mut self, expr: Rc<RExpr<'tcx>>) -> Result<AnalysisType, AnalysisError> {
        use RExprKind::*;

        let mut return_value = AnalysisType::Other;

        match expr.kind.clone() {
            Literal { .. } => self.analyze_literal(expr)?,
            VarRef { .. } => self.analyze_var_ref(expr)?,
            Binary { .. } => self.analyze_binary(expr)?,
            Pat { kind } => self.analyze_pat(&kind, expr)?,
            Call { ty, args, .. } => self.analyze_fn(ty, &*args, expr)?,
            LetStmt { pattern, initializer, else_block } => {
                self.analyze_let_stmt(pattern, initializer, else_block)?
            }
            Return { value } => {
                if let Some(expr) = value {
                    return_value = AnalysisType::Return(Some(self.expr_to_constraint(expr)?));
                } else {
                    return_value = AnalysisType::Return(None);
                }
            }
            AssignOp { op, lhs, rhs } => self.analyze_assign_op(op, lhs, rhs, expr)?,
            Assign { lhs, rhs } => self.analyze_assign(lhs, rhs, expr)?,
            If { cond, then, else_opt } => self.analyze_if(cond, then, else_opt)?,
            _ => {
                println!("{:?}", expr.kind);
                return Err(AnalysisError::UnsupportedPattern("Unknown expr".into()));
            }
        }

        Ok(return_value)
    }

    /// Sub analyze functions
    /// - analyze_literal
    /// - analyze_var_ref
    /// - analyze_binary
    /// - analyze_pat
    /// - analyze_fn
    /// - analyze_extern_fn
    /// - analyze_let_stmt
    /// - analyze_assign_op
    /// - analyze_assign
    /// - analyze_if
    /// - analyze_block

    fn analyze_literal(&mut self, expr: Rc<RExpr<'tcx>>) -> Result<(), AnalysisError> {
        let constraint = self.expr_to_constraint(expr.clone())?;
        self.add_assumption(constraint, expr);
        Ok(())
    }

    fn analyze_var_ref(&mut self, expr: Rc<RExpr<'tcx>>) -> Result<(), AnalysisError> {
        let constraint = self.expr_to_constraint(expr.clone())?;
        self.add_assumption(constraint, expr);
        Ok(())
    }

    fn analyze_binary(&mut self, expr: Rc<RExpr<'tcx>>) -> Result<(), AnalysisError> {
        let constraint = self.expr_to_constraint(expr.clone())?;
        self.add_assumption(constraint, expr);
        Ok(())
    }

    fn analyze_pat(
        &mut self, kind: &RPatKind<'tcx>, pat: Rc<RExpr<'tcx>>,
    ) -> Result<(), AnalysisError> {
        use RPatKind::*;

        match kind {
            Wild => (),
            Binding { name, ty, var, .. } => self.add_parameter(name.to_string(), ty, var, pat),
            _ => return Err(AnalysisError::UnsupportedPattern(format!("{:?}", kind))),
        }
        Ok(())
    }

    fn analyze_fn(
        &mut self, ty: Ty<'tcx>, args: &[Rc<RExpr<'tcx>>], expr: Rc<RExpr<'tcx>>,
    ) -> Result<(), AnalysisError> {
        match ty.kind() {
            TyKind::FnDef(def_id, ..) => {
                let mut fn_info = self.get_fn_info(def_id);
                if let Some(fun) = self.get_local_fn(def_id) {
                    self.save_and_switch_ctxt(fn_info.pop().expect("fn info not found"), expr)?;
                    match self.analyze_local_fn(fun, args) {
                        Ok(()) => {
                            let fn_ctxt = self.restore_ctxt();
                            self.merge_ctxt(fn_ctxt);
                            Ok(())
                        }
                        Err(why) => Err(why),
                    }
                } else {
                    self.analyze_extern_fn(fn_info, args)
                }
            }
            _ => panic!("Call has not have FnDef"),
        }
    }

    fn analyze_extern_fn(
        &mut self, fn_info: Vec<String>, args: &[Rc<RExpr<'tcx>>],
    ) -> Result<(), AnalysisError> {
        if fn_info[0] == "t3modules" {
            match fn_info[1].as_str() {
                "t3assert" => self.analyze_t3assert(args),
                "t3assume" => self.analyze_t3assume(args),
                _ => unreachable!(),
            }
        } else {
            Err(AnalysisError::UnsupportedPattern("Unknown function!".into()))
        }
    }

    fn analyze_let_stmt(
        &mut self, pattern: Rc<RExpr<'tcx>>, initializer: Option<Rc<RExpr<'tcx>>>,
        _: Option<Rc<RExpr<'tcx>>>,
    ) -> Result<(), AnalysisError> {
        if let RExprKind::Pat { kind: RPatKind::Binding { name, ty, var, .. } } = &pattern.kind {
            let name = format!("{}_{}", self.current_context.name(), name);
            let declaration = Lir::new_parameter(name.clone(), ty.clone(), pattern.clone());
            self.current_context.add_assumption(declaration);
            self.current_context.insert_var(var, name.clone(), ty);
            if let Some(init) = initializer {
                match self.expr_to_constraint(init.clone()) {
                    Ok(value) => {
                        self.add_assumption(format!("(= {} {})", name, value), init.clone())
                    }
                    Err(err) => match err {
                        AnalysisError::RandFunctions => {}
                        _ => return Err(err),
                    },
                }
            }
        } else {
            unreachable!();
        }
        Ok(())
    }

    fn analyze_assign_op(
        &mut self, op: BinOp, lhs: Rc<RExpr<'tcx>>, rhs: Rc<RExpr<'tcx>>, expr: Rc<RExpr<'tcx>>,
    ) -> Result<(), AnalysisError> {
        let rhs = self.expr_to_constraint(rhs)?;
        let (new_lhs, lhs) = self.assign_new_value(lhs.clone())?;
        let constraint = self.bin_op_to_constraint(op, &lhs, &rhs)?;
        self.add_assumption(format!("(= {} {})", new_lhs, constraint), expr);
        Ok(())
    }

    fn analyze_assign(
        &mut self, lhs: Rc<RExpr<'tcx>>, rhs: Rc<RExpr<'tcx>>, expr: Rc<RExpr<'tcx>>,
    ) -> Result<(), AnalysisError> {
        let rhs = self.expr_to_constraint(rhs.clone())?;
        let (new_lhs, _) = self.assign_new_value(lhs.clone())?;
        let new_assume = Lir::new_assume(format!("(= {} {})", new_lhs, rhs), expr.clone());
        self.current_context.add_assumption(new_assume);
        Ok(())
    }

    fn analyze_if(
        &mut self, cond: Rc<RExpr<'tcx>>, then_block: Rc<RExpr<'tcx>>,
        else_opt: Option<Rc<RExpr<'tcx>>>,
    ) -> Result<(), AnalysisError> {
        let cond_str = self.expr_to_constraint(cond.clone())?;

        self.save_and_switch_ctxt(
            if &self.current_context.name() == "then" {
                format!("{}+", self.current_context.name())
            } else {
                "then".to_string()
            },
            then_block.clone(),
        )?;
        let cond_constraint = Lir::new_assume(cond_str.clone(), cond.clone());
        self.current_context.add_assumption(cond_constraint);
        self.analyze_block(then_block)?;

        let then_ctxt = self.restore_ctxt();

        let mut else_ctxt = None;
        if let Some(else_block) = else_opt {
            self.save_and_switch_ctxt(
                if &self.current_context.name() == "else" {
                    format!("{}+", self.current_context.name())
                } else {
                    "else".to_string()
                },
                else_block.clone(),
            )?;
            let cond_constraint = Lir::new_assume(format!("(not {})", cond_str.clone()), cond);
            self.current_context.add_assumption(cond_constraint);
            self.analyze_block(else_block)?;
            else_ctxt = Some(self.restore_ctxt());
        }

        self.merge_then_else_ctxt(cond_str.clone(), then_ctxt, else_ctxt)?;
        Ok(())
    }

    fn analyze_block(&mut self, block: Rc<RExpr<'tcx>>) -> Result<(), AnalysisError> {
        if let RExpr { kind: RExprKind::Block { stmts, .. }, .. } = &*block {
            for stmt in stmts {
                self.analyze_expr(stmt.clone())?;
            }
        } else {
            return Err(AnalysisError::UnsupportedPattern("Unknown body pattern".into()));
        }
        Ok(())
    }

    /// String convertor functions
    /// - expr_to_constraint
    /// - literal_to_constraint
    /// - var_ref_to_constraint
    /// - logical_op_to_constraint
    /// - bin_op_to_constraint
    /// - fn_to_constraint
    /// - extern_fn_to_constraint
    /// - value_to_ite
    /// - block_to_constraint

    fn expr_to_constraint(&mut self, arg: Rc<RExpr<'tcx>>) -> Result<String, AnalysisError> {
        use RExprKind::*;

        match &arg.kind {
            Literal { lit, neg } => Ok(Analyzer::literal_to_constraint(lit, *neg)?),
            VarRef { id } => self.var_ref_to_constraint(id),
            LogicalOp { op, lhs, rhs } => {
                let lhs_str = self.expr_to_constraint(lhs.clone())?;
                let rhs_str = self.expr_to_constraint(rhs.clone())?;
                Ok(self.logical_op_to_constraint(*op, &lhs_str, &rhs_str)?)
            }
            Unary { op, arg } => {
                let arg_str = self.expr_to_constraint(arg.clone())?;
                Ok(self.un_op_to_constraint(*op, &arg_str)?)
            }
            Binary { op, lhs, rhs } => {
                let lhs_str = self.expr_to_constraint(lhs.clone())?;
                let rhs_str = self.expr_to_constraint(rhs.clone())?;
                Ok(self.bin_op_to_constraint(*op, &lhs_str, &rhs_str)?)
            }
            Call { ty, args, .. } => self.fn_to_constraint(*ty, args),
            If { cond, then, else_opt } => {
                Ok(self.if_to_constraint(cond.clone(), then.clone(), else_opt.clone())?)
            }
            _ => {
                println!("{}", self.get_current_assumptions()?);
                Err(AnalysisError::UnsupportedPattern(format!("name: {:?}", arg.kind)))
            }
        }
    }

    fn literal_to_constraint(lit: &'tcx Lit, neg: bool) -> Result<String, AnalysisError> {
        match lit.node {
            LitKind::Str(symbol, _) => Ok(symbol.to_string()),
            LitKind::Char(c) => Ok(format!("'{}'", c)),
            LitKind::Int(n, _) => Ok(if neg { format!("-{}", n) } else { format!("{}", n) }),
            LitKind::Float(symbol, _) => {
                Ok(if neg { format!("-{}", symbol) } else { format!("{}", symbol) })
            }
            LitKind::Bool(b) => Ok(b.to_string()),
            LitKind::ByteStr(ref bytes, _) => {
                Ok(format!("b\"{}\"", String::from_utf8_lossy(bytes)))
            }
            _ => Err(AnalysisError::UnsupportedPattern(format!(
                "Unsupported literal pattern: {}",
                lit.node
            ))),
        }
    }

    fn var_ref_to_constraint(&self, id: &LocalVarId) -> Result<String, AnalysisError> {
        Ok(self.get_var(*id))
    }

    fn logical_op_to_constraint(
        &mut self, op: LogicalOp, lhs_str: &String, rhs_str: &String,
    ) -> Result<String, AnalysisError> {
        use LogicalOp::*;

        let op_str = match op {
            And => "and",
            Or => "or",
        };
        Ok(format!("({} {} {})", op_str, lhs_str, rhs_str))
    }

    fn un_op_to_constraint(&mut self, op: UnOp, arg_str: &String) -> Result<String, AnalysisError> {
        use UnOp::*;

        let op_str = match op {
            Not => "not",
            Neg => "-",
            _ => return Err(AnalysisError::UnsupportedPattern(format!("{:?}", op))),
        };
        Ok(format!("({} {})", op_str, arg_str))
    }

    fn bin_op_to_constraint(
        &mut self, op: BinOp, lhs_str: &String, rhs_str: &String,
    ) -> Result<String, AnalysisError> {
        use BinOp::*;

        let op_str = match op {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Rem => "%",
            Div => "/",
            BitXor => "^",
            BitAnd => "&",
            BitOr => "|",
            Eq => "=",
            Lt => "<",
            Le => "<=",
            Ne => "!=",
            Ge => ">=",
            Gt => ">",
            _ => return Err(AnalysisError::UnsupportedPattern(format!("{:?}", op))),
        };
        Ok(format!("({} {} {})", op_str, lhs_str, rhs_str))
    }

    fn fn_to_constraint(
        &mut self, ty: Ty<'tcx>, args: &[Rc<RExpr<'tcx>>],
    ) -> Result<String, AnalysisError> {
        match ty.kind() {
            TyKind::FnDef(def_id, ..) => {
                let fn_info = self.get_fn_info(def_id);
                if let Some(fun) = self.get_local_fn(def_id) {
                    self.local_fn_to_constraint(fun.clone(), args)
                } else {
                    self.extern_fn_to_constraint(fn_info, args)
                }
            }
            _ => panic!("Call has not have FnDef"),
        }
    }

    fn local_fn_to_constraint(
        &mut self, expr: Rc<RThir<'tcx>>, args: &[Rc<RExpr<'tcx>>],
    ) -> Result<String, AnalysisError> {
        self.analyze_params(&expr.params, args)?;
        self.block_to_constraint(expr.body.as_ref().expect("Body not found").clone())
    }

    fn extern_fn_to_constraint(
        &self, fn_info: Vec<String>, _: &[Rc<RExpr<'tcx>>],
    ) -> Result<String, AnalysisError> {
        if fn_info[0] == "t3modules" {
            match fn_info[1].as_str() {
                "rand_bool" => Err(AnalysisError::RandFunctions),
                "rand_int" => Err(AnalysisError::RandFunctions),
                "rand_float" => Err(AnalysisError::RandFunctions),
                _ => unreachable!(),
            }
        } else {
            Err(AnalysisError::UnsupportedPattern("Unknown function!".into()))
        }
    }

    fn if_to_constraint(
        &mut self, cond: Rc<RExpr<'tcx>>, then_block: Rc<RExpr<'tcx>>,
        else_opt: Option<Rc<RExpr<'tcx>>>,
    ) -> Result<String, AnalysisError> {
        let cond_str = self.expr_to_constraint(cond.clone())?;

        self.save_and_switch_ctxt(
            if &self.current_context.name() == "then" {
                format!("{}+", self.current_context.name())
            } else {
                "then".to_string()
            },
            then_block.clone(),
        )?;
        let cond_constraint = Lir::new_assume(cond_str.clone(), cond.clone());
        self.current_context.add_assumption(cond_constraint);
        let then_value = self.block_to_constraint(then_block)?;

        let then_ctxt = self.restore_ctxt();

        let else_block = else_opt.expect("Else block of if initializer not found");
        self.save_and_switch_ctxt(
            if &self.current_context.name() == "else" {
                format!("{}+", self.current_context.name())
            } else {
                "else".to_string()
            },
            else_block.clone(),
        )?;
        let cond_constraint = Lir::new_assume(format!("(not {})", cond_str.clone()), cond);
        self.current_context.add_assumption(cond_constraint);
        let else_value = self.block_to_constraint(else_block)?;
        let else_ctxt = Some(self.restore_ctxt());

        self.merge_then_else_ctxt(cond_str.clone(), then_ctxt, else_ctxt)?;
        Ok(Analyzer::value_to_ite(cond_str, then_value, else_value))
    }

    fn value_to_ite(cond_str: String, then_value: String, else_value: String) -> String {
        format!("(ite {} {} {})", cond_str, then_value, else_value)
    }

    fn block_to_constraint(&mut self, block: Rc<RExpr<'tcx>>) -> Result<String, AnalysisError> {
        let mut return_value = String::new();
        if let RExpr { kind: RExprKind::Block { stmts, expr }, .. } = &*block {
            for stmt in stmts {
                if let AnalysisType::Return(value) = self.analyze_expr(stmt.clone())? {
                    return Ok(value.expect("No value with return"));
                }
            }
            if let Some(expr) = expr {
                return_value = self.expr_to_constraint(expr.clone())?;
            }
        } else {
            return Err(AnalysisError::UnsupportedPattern("Unknown body pattern".into()));
        }
        Ok(return_value)
    }

    /// Manage context functions
    /// - merge_ctxt
    /// - save_and_switch_ctxt
    /// - restore_ctxt
    /// - merge_then_else_ctxt
    /// - adapt_cond_to_path

    fn merge_ctxt(&mut self, ctxt: Context<'tcx>) {
        for assumption in ctxt.path.iter() {
            match assumption.kind {
                LirKind::Declaration { .. } => {
                    self.current_context.add_assumption(assumption.clone())
                }
                LirKind::Assume(_) => self.current_context.add_assumption(assumption.clone()),
                _ => (),
            }
        }

        let mut new_var_map = Map::new();
        let current_var_map = self.current_context.var_map.clone();
        for (var_id, (var_str, ty)) in current_var_map.iter() {
            if let Some((ctxt_var_str, ..)) = ctxt.var_map.get(var_id) {
                if var_str != ctxt_var_str {
                    let new_var_str = format!("{}+", var_str);
                    self.current_context.add_assumption(Lir::new_parameter(
                        new_var_str.clone(),
                        ty.clone(),
                        ctxt.path.back().expect("No lir found in new_var_map").expr.clone(),
                    ));
                    self.current_context.add_assumption(Lir::new_assume(
                        format!("(= {} {})", new_var_str, var_str),
                        ctxt.path.back().expect("No lir found in new_var_map").expr.clone(),
                    ));
                    self.current_context.add_assumption(Lir::new_assume(
                        format!("(= {} {})", new_var_str, ctxt_var_str),
                        ctxt.path.back().expect("No lir found in new_var_map").expr.clone(),
                    ));
                    new_var_map.insert(var_id.clone(), (new_var_str, ty.clone()));
                } else {
                    new_var_map.insert(var_id.clone(), (var_str.clone(), ty.clone()));
                }
            }
        }
        self.current_context.var_map = new_var_map;
    }

    fn save_and_switch_ctxt(
        &mut self, name: String, expr: Rc<RExpr<'tcx>>,
    ) -> Result<(), AnalysisError> {
        self.context_stack.push(self.current_context.clone());
        let assumptions = Lir::new_assumptions(self.get_current_assumptions()?, expr);
        let mut new_path = VecDeque::new();
        new_path.push_back(assumptions);
        self.current_context = Context::from(name, new_path, self.current_context.var_map.clone());
        Ok(())
    }

    fn restore_ctxt(&mut self) -> Context<'tcx> {
        let current_ctxt = self.current_context.clone();
        self.current_context = self.context_stack.pop().expect("No context found");
        current_ctxt
    }

    fn merge_then_else_ctxt(
        &mut self, cond_str: String, then_ctxt: Context<'tcx>, else_ctxt: Option<Context<'tcx>>,
    ) -> Result<(), AnalysisError> {
        let then_ctxt = Analyzer::adapt_cond_to_path(&cond_str, then_ctxt)?;
        self.merge_ctxt(then_ctxt);
        if let Some(else_ctxt) = else_ctxt {
            let else_ctxt =
                Analyzer::adapt_cond_to_path(&format!("(not {})", cond_str), else_ctxt)?;
            self.merge_ctxt(else_ctxt);
        }
        Ok(())
    }

    fn adapt_cond_to_path(
        cond_str: &String, mut ctxt: Context<'tcx>,
    ) -> Result<Context<'tcx>, AnalysisError> {
        let mut adapted_path = VecDeque::new();
        let len = ctxt.path.len();
        for i in 0..len {
            let lir = ctxt.path[i].clone();
            match lir.kind {
                LirKind::Declaration { .. } => adapted_path.push_back(lir),
                LirKind::Assume(constraint) => {
                    if &constraint != cond_str {
                        adapted_path.push_back(Lir::new_assume(
                            format!("(=> {} {})", cond_str, constraint),
                            lir.expr.clone(),
                        ));
                    }
                }
                _ => (),
            }
        }
        ctxt.path = adapted_path;
        Ok(ctxt)
    }

    /// Special functions
    /// - analyze_t3assert
    /// - analyze_t3assume

    fn analyze_t3assert(&mut self, args: &[Rc<RExpr<'tcx>>]) -> Result<(), AnalysisError> {
        self.analyze_t3assume(args)?;
        self.verify()
    }

    fn analyze_t3assume(&mut self, args: &[Rc<RExpr<'tcx>>]) -> Result<(), AnalysisError> {
        let constraint = self.expr_to_constraint(args[0].clone())?;
        self.add_assumption(constraint, args[0].clone());
        Ok(())
    }
}
