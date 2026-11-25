use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use super::coverage::Target;
use super::data::{Data, Mirror};
use super::evaluate::Value;
use super::family::{Decision, READERS};
use super::layers::Config;

#[derive(Default)]
struct Evaluator {
    config: Config,
    mirrored: HashMap<usize, (Value, Data)>,
}

// SAFETY: reached only through EVALUATOR's lock, every operation returns owned copies, nothing in the Rc graph points at thread-local state, and the config ASTs it reaches sit in arenas that are never freed and that nothing allocates into or writes after parsing.
unsafe impl Send for Evaluator {}

static EVALUATOR: LazyLock<Mutex<Evaluator>> = LazyLock::new(Mutex::default);

fn evaluated<R: Send>(work: impl FnOnce(&mut Evaluator) -> R + Send) -> R {
    work(&mut EVALUATOR.lock().unwrap_or_else(PoisonError::into_inner))
}

impl Evaluator {
    fn mirror(&mut self, value: &Value) -> Data {
        let address = match value {
            Value::Undefined => return Data::Undefined,
            Value::Null => return Data::Null,
            Value::Bool(value) => return Data::Bool(*value),
            Value::Number(value) => return Data::Number(*value),
            Value::String(text) => return Data::String(Arc::from(&**text)),
            Value::Unknown => return Data::Unknown,
            Value::Array(items) => Rc::as_ptr(items).addr(),
            Value::Object(object) => Rc::as_ptr(object).addr(),
        };

        if let Some((_, known)) = self.mirrored.get(&address) {
            return known.clone();
        }

        let data = match value {
            Value::Array(items) => Data::Array(Arc::new(
                items.iter().map(|item| self.mirror(item)).collect(),
            )),
            Value::Object(object) => Data::Object(Arc::new(Mirror::lazy(
                address,
                object.answers_every_key(),
                object.keys(),
            ))),
            _ => unreachable!("scalars return above"),
        };

        self.mirrored.insert(address, (value.clone(), data.clone()));
        data
    }
}

pub fn force(address: usize, key: &str) -> Data {
    evaluated(|evaluator| {
        let Some((Value::Object(object), _)) = evaluator.mirrored.get(&address) else {
            unreachable!("a lazy mirror's address maps to its object")
        };

        let value = object.get(&evaluator.config.session, key);
        evaluator.mirror(&value)
    })
}

pub fn decisions(target: &Target) -> (Vec<Decision>, bool) {
    evaluated(|evaluator| {
        let config = &evaluator.config;
        config.varies.set(false);
        let decisions = READERS
            .into_iter()
            .filter_map(|reader| reader.decide(config, target))
            .collect();

        (decisions, config.varies.get())
    })
}

pub fn exported_default(file: &str) -> Data {
    evaluated(|evaluator| {
        let session = &evaluator.config.session;
        session.depth.begin_root(file);
        let value = session.exported(file, "default", &Rc::default());

        evaluator.mirror(&value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::javascript::config::data::property;

    #[test]
    fn mirrors_keep_one_view_per_value() {
        let objects = (0..1000)
            .map(|index| format!("{{ index: {index} }}"))
            .collect::<Vec<_>>()
            .join(", ");

        let root = super::super::tests::dir_with(&[
            (
                "shared.mjs",
                "const shared = { printWidth: 91, tabWidth: 3 };\nexport default { one: shared, two: shared };\n",
            ),
            ("many.mjs", &format!("export default [{objects}];\n")),
        ]);

        let shared = || exported_default(&root.join("shared.mjs"));
        let mirror = |key| match property(&shared(), key) {
            Data::Object(mirror) => mirror,
            other => panic!("{key} mirrors as {other:?}"),
        };

        let number = |mirror: &Mirror, key| match mirror.get(key) {
            Data::Number(number) => number,
            other => panic!("{key} mirrors as {other:?}"),
        };

        let one = mirror("one");
        assert!(Arc::ptr_eq(&one, &mirror("two")));
        assert_eq!(number(&one, "printWidth"), 91.0);

        let Data::Array(many) = exported_default(&root.join("many.mjs")) else {
            panic!("many.mjs mirrors as no array");
        };

        assert_eq!(many.len(), 1000);
        assert!(Arc::ptr_eq(&one, &mirror("one")));
        assert_eq!(number(&one, "printWidth"), 91.0);
        assert_eq!(number(&one, "tabWidth"), 3.0);
    }

    #[test]
    fn a_mirrored_array_outlives_its_source() {
        let mut evaluator = Evaluator::default();
        let items = Rc::new(vec![Value::Number(1.0)]);
        let source = Rc::downgrade(&items);

        evaluator.mirror(&Value::Array(items));

        assert!(source.upgrade().is_some());
    }
}
