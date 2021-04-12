#![feature(option_unwrap_none)]
#![feature(box_patterns)]

mod ast;
mod bytecode;
mod script;
mod tokenizer;

use rustc_hash::FxHashMap;
use script::Script;
pub use script::{Class, ScriptIter, ScriptType};

use bytecode::ByteCode;
use tokenizer::TokenStream;

pub fn parse(source: &str) -> Result<Class, ()> {
    println!("Source:\n---\n{}\n---", source);
    let tks = TokenStream::parse(source).unwrap();
    dbg!(&tks);
    let ast = ast::Script::parse(tks).unwrap();

    let locals = {
        let locals = ast.variables;
        let mut hm = FxHashMap::with_capacity_and_hasher(locals.len(), Default::default());
        for (i, l) in locals.iter().enumerate() {
            if hm.insert(l.to_string().into(), i as u16).is_some() {
                panic!("Duplicate local");
                //return Err(ByteCodeError::DuplicateLocal);
            }
        }
        hm.shrink_to_fit();
        hm
    };

    let mut script = Script::new(locals);

    for f in ast.functions {
        let name = f.name.into();
        match ByteCode::parse(f, &script.locals) {
            Ok(f) => {
                script.functions.insert(name, f);
            }
            Err(e) => todo!("{:?}", e),
        }
    }
    script.functions.shrink_to_fit();

    Ok(script.into())
}
