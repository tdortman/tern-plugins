//! The parts of a `.drv` file (`ATerm`) the dependency tree needs.

/// What a derivation file says about itself.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrvInfo {
    /// `.drv` paths of its input derivations.
    pub inputs: Vec<String>,
    /// Its `pname`, when the derivation sets one.
    pub pname: Option<String>,
}

/// One `ATerm` value.
#[derive(Debug)]
enum Term {
    Str(String),
    List(Vec<Self>),
    /// A tuple, or a constructor application (`Derive(…)`).
    App(Vec<Self>),
}

/// A tiny recursive-descent `ATerm` reader.
struct Parser<'a> {
    rest: &'a str,
}

impl Parser<'_> {
    fn eat(&mut self, c: char) -> Option<()> {
        self.rest = self.rest.strip_prefix(c)?;
        Some(())
    }

    fn term(&mut self) -> Option<Term> {
        match self.rest.chars().next()? {
            '"' => self.string().map(Term::Str),
            '[' => self.seq('[', ']').map(Term::List),
            '(' => self.seq('(', ')').map(Term::App),
            _ => {
                // A constructor name such as `Derive`, then its arguments.
                let end = self.rest.find('(')?;
                self.rest = &self.rest[end..];
                self.seq('(', ')').map(Term::App)
            }
        }
    }

    fn seq(&mut self, open: char, close: char) -> Option<Vec<Term>> {
        self.eat(open)?;
        let mut items = Vec::new();
        if self.eat(close).is_some() {
            return Some(items);
        }
        loop {
            items.push(self.term()?);
            if self.eat(close).is_some() {
                return Some(items);
            }
            self.eat(',')?;
        }
    }

    fn string(&mut self) -> Option<String> {
        self.eat('"')?;
        let mut out = String::new();
        let mut chars = self.rest.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    self.rest = &self.rest[i + 1..];
                    return Some(out);
                }
                '\\' => match chars.next()?.1 {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    other => out.push(other),
                },
                other => out.push(other),
            }
        }
        None
    }
}

/// Reads `Derive(outputs, inputDrvs, inputSrcs, system, builder, args, env)`.
pub fn parse(text: &str) -> Option<DrvInfo> {
    let Term::App(fields) = (Parser { rest: text.trim() }).term()? else {
        return None;
    };
    let mut info = DrvInfo::default();
    if let Some(Term::List(inputs)) = fields.get(1) {
        for input in inputs {
            if let Term::App(pair) = input
                && let Some(Term::Str(path)) = pair.first()
            {
                info.inputs.push(path.clone());
            }
        }
    }
    if let Some(Term::List(env)) = fields.get(6) {
        for entry in env {
            if let Term::App(pair) = entry
                && let [Term::Str(k), Term::Str(v)] = pair.as_slice()
                && k == "pname"
            {
                info.pname = Some(v.clone());
            }
        }
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_inputs_and_pname_through_escapes() {
        let drv = r#"Derive([("out","/s/aaa-hello-2.12","","")],[("/s/bbb-bash-5.drv",["out"]),("/s/ccc-gcc-14.drv",["lib","out"])],["/s/ddd-builder.sh"],"x86_64-linux","/s/bash",["-e","/s/ddd-builder.sh"],[("buildPhase","echo \"a,b\"\n(x)"),("name","hello-2.12"),("pname","hello")])"#;
        assert_eq!(
            parse(drv),
            Some(DrvInfo {
                inputs: vec!["/s/bbb-bash-5.drv".into(), "/s/ccc-gcc-14.drv".into()],
                pname: Some("hello".into()),
            })
        );
    }

    #[test]
    fn rejects_truncated_files() {
        assert_eq!(parse(r#"Derive([("out","/s/aaa"#), None);
    }
}
