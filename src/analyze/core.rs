// rustc crates
// std crates
// Own crates
use crate::analyze::*;

impl<'tcx> Analyzer<'tcx> {
    pub fn analyze_main(&self, rthir: Rc<RThir<'tcx>>) -> Result<(), AnalysisError> {
        if let Some(body) = &rthir.body {
            let mut main_env = Env::new();
            self.analyze_body((*body).clone(), &mut main_env)?;
        }
        Ok(())
    }

    pub fn analyze_params(
        &self, params: &Vec<RParam<'tcx>>, args: Box<[Rc<RExpr<'tcx>>]>, env: &mut Env<'tcx>,
    ) -> Result<(), AnalysisError> {
        use RExprKind::*;
        use RPatKind::*;

        for (param, arg) in params.iter().zip(args.iter()) {
            if let Some(pat) = &param.pat {
                if let RExpr { kind: Pat { kind }, .. } = pat.as_ref() {
                    match kind {
                        Binding { name, ty, var, .. } => {
                            let env_name = format!("{}_{}", env.name, name);
                            env.add_parameter(env_name.clone(), ty, var, pat.clone());
                            let value = self.expr_to_constraint(arg.clone(), env)?;
                            env.add_assumption(format!("(= {} {})", env_name, value), arg.clone());
                        }
                        Wild => (),
                        _ => return Err(AnalysisError::UnsupportedPattern(format!("{:?}", kind))),
                    }
                }
            }
        }
        Ok(())
    }

    pub fn analyze_body(
        &self, body: Rc<RExpr<'tcx>>, env: &mut Env<'tcx>,
    ) -> Result<(), AnalysisError> {
        if let RExpr { kind: RExprKind::Block { stmts, expr }, .. } = body.as_ref() {
            let mut stmts_iter = stmts.iter().cloned().peekable();
            while let Some(stmt) = stmts_iter.next() {
                let return_value = self.analyze_expr(stmt.clone(), env)?;
                match &return_value {
                    AnalysisType::Return(..) => break,
                    AnalysisType::Invariant(expr) => {
                        self.analyze_loop(expr.clone(), &mut stmts_iter, env)?
                    }
                    AnalysisType::Other => (),
                }
            }
            if let Some(expr) = expr {
                self.analyze_expr(expr.clone(), env)?;
            }
        } else {
            return Err(AnalysisError::UnsupportedPattern("Unknown body pattern".into()));
        }
        Ok(())
    }

    pub fn analyze_expr(
        &self, expr: Rc<RExpr<'tcx>>, env: &mut Env<'tcx>,
    ) -> Result<AnalysisType<'tcx>, AnalysisError> {
        use RExprKind::*;

        let mut return_value = AnalysisType::Other;

        match expr.kind.clone() {
            Literal { .. } => self.analyze_literal(expr, env)?,
            VarRef { .. } => self.analyze_var_ref(expr, env)?,
            Binary { .. } => self.analyze_binary(expr, env)?,
            Pat { kind } => self.analyze_pat(&kind, expr, env)?,
            Call { ty, args, .. } => return_value = self.analyze_fn(ty, args, expr, env)?,
            LetStmt { pattern, initializer, else_block } => {
                self.analyze_let_stmt(pattern, initializer, else_block, env)?
            }
            Return { value } => {
                if let Some(expr) = value {
                    return_value = AnalysisType::Return(Some(self.expr_to_constraint(expr, env)?));
                } else {
                    return_value = AnalysisType::Return(None);
                }
            }
            AssignOp { op, lhs, rhs } => self.analyze_assign_op(op, lhs, rhs, expr, env)?,
            Assign { lhs, rhs } => self.analyze_assign(lhs, rhs, expr, env)?,
            If { cond, then, else_opt } => self.analyze_if(cond, then, else_opt, env)?,
            // Break { .. } => (),
            _ => {
                println!("{:?}", expr.kind);
                return Err(AnalysisError::UnsupportedPattern("Unknown expr".into()));
            }
        }
        Ok(return_value)
    }
}
