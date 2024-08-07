// rustc crates
use rustc_span::def_id::{DefId, LocalDefId};

// std crates
use std::io::Write;
use std::process::Command;

// Own crates
use crate::analyze::*;
impl<'tcx> Analyzer<'tcx> {
    pub fn verify(&self, mut smt: String, env: &Env<'tcx>) -> Result<(), AnalysisError> {
        let mut child = Command::new("z3")
            .args(["-in", "-model"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("Run z3 failed");

        let mut stdin = child.stdin.take().expect("Open std failed");
        smt += "(check-sat)\n";
        println!("{}", smt);
        stdin.write_all(smt.as_bytes()).expect("Write smt failed");
        drop(stdin);

        let output = child.wait_with_output().expect("Get stdout failed");
        let result = String::from_utf8(output.stdout).expect("Load result failed");
        if &result != "unsat\n" {
            return Err(AnalysisError::VerifyError { span: env.get_latest_span() });
        }

        println!("Verification success!\n");

        Ok(())
    }

    pub fn get_fn(&self, fn_id: LocalDefId) -> Result<Rc<RThir<'tcx>>, AnalysisError> {
        self.fn_map.get(&fn_id).cloned().ok_or(AnalysisError::FunctionNotFound(fn_id))
    }

    pub fn get_local_fn(&self, def_id: &DefId) -> Option<Rc<RThir<'tcx>>> {
        if def_id.is_local() {
            Some(self.fn_map.get(&def_id.expect_local()).expect("Get local fn failed").clone())
        } else {
            None
        }
    }

    pub fn get_fn_info(&self, def_id: &DefId) -> Vec<String> {
        let def_path = self.tcx.def_path_str(*def_id);
        def_path
            .split(|c| c == ':' || c == '"' || c == '\\')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    }
}
