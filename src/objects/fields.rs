use super::*;

const ABSENT: u32 = u32::MAX;
const END: u32 = u32::MAX - 1;

pub(super) enum Fields {
    Named(Hash),
    Slots {
        values: Buffer<Slot>,
        first: u32,
        last: u32,
    },
}

pub(super) struct Slot {
    value: Value,
    // First-write order is independent of slot order. ABSENT distinguishes an
    // unassigned field from an assigned nil, including across host imports.
    previous: u32,
    next: u32,
}

impl Fields {
    pub fn new(class: &Namespace) -> Self {
        if class.field_layout().is_some() {
            Self::Slots {
                values: Buffer::empty(),
                first: END,
                last: END,
            }
        } else {
            Self::Named(Hash::empty())
        }
    }

    pub fn find(
        &self,
        ctx: &mut CallContext,
        class: &Namespace,
        name: &str,
    ) -> Result<Option<usize>> {
        match self {
            Self::Named(named) => named.find(ctx, name.as_bytes()),
            Self::Slots { values, .. } => {
                for (slot, field) in class.field_layout().unwrap().iter().enumerate() {
                    ctx.charge(1)?;
                    if field == name {
                        return Ok(values
                            .data
                            .get(slot)
                            .filter(|v| v.next != ABSENT)
                            .map(|_| slot));
                    }
                }
                Ok(None)
            }
        }
    }

    pub fn get(&self, slot: usize) -> Option<&Value> {
        match self {
            Self::Named(named) => named.buffer.data.get(slot).map(|(_, v)| v),
            Self::Slots { values, .. } => values
                .data
                .get(slot)
                .filter(|v| v.next != ABSENT)
                .map(|v| &v.value),
        }
    }

    pub fn set(
        &mut self,
        ctx: &mut CallContext,
        class: &Namespace,
        name: &str,
        value: Value,
    ) -> Result<()> {
        match self {
            Self::Named(named) => named.insert_named_field(ctx, name.as_bytes(), value),
            Self::Slots { .. } => {
                let slot = class
                    .field_layout()
                    .unwrap()
                    .iter()
                    .position(|field| field == name)
                    .ok_or_else(|| Error::new(ErrorKind::Name, "undeclared instance variable"))?;
                self.set_slot(ctx, slot, value)
            }
        }
    }

    pub fn set_slot(&mut self, ctx: &mut CallContext, slot: usize, value: Value) -> Result<()> {
        match self {
            Self::Named(named) => {
                let name = named.buffer.data[slot].0.clone();
                named.insert_field(ctx, name, value)
            }
            Self::Slots {
                values,
                first,
                last,
            } => {
                ctx.charge(1)?;
                if slot >= values.data.len() {
                    values.ensure(ctx, slot + 1)?;
                    while values.data.len() < slot {
                        let end =
                            slot.min(values.data.len() + crate::budget::CHUNK / size_of::<Slot>());
                        ctx.work_bytes((end - values.data.len()) * size_of::<Slot>())?;
                        values.data.resize_with(end, || Slot {
                            value: Value::nil(),
                            previous: ABSENT,
                            next: ABSENT,
                        });
                    }
                    // The selected slot's write is charged above; only skipped
                    // slots add initialization work.
                    values.data.push(Slot {
                        value: Value::nil(),
                        previous: ABSENT,
                        next: ABSENT,
                    });
                }
                if values.data[slot].next == ABSENT {
                    if *last == END {
                        *first = slot as u32;
                    } else {
                        values.data[*last as usize].next = slot as u32;
                    }
                    values.data[slot].previous = *last;
                    values.data[slot].next = END;
                    *last = slot as u32;
                }
                values.data[slot].value = value;
                Ok(())
            }
        }
    }

    pub fn name<'a>(&'a self, class: &'a Namespace, slot: usize) -> &'a [u8] {
        match self {
            Self::Named(named) => named.buffer.data[slot].0.as_bytes().unwrap(),
            Self::Slots { .. } => class.field_layout().unwrap()[slot].as_bytes(),
        }
    }

    pub fn names<'a>(&'a self, class: &'a Namespace) -> impl Iterator<Item = &'a [u8]> {
        self.iter().map(move |(slot, _)| self.name(class, slot))
    }

    pub fn iter(&self) -> Iter<'_> {
        match self {
            Self::Named(named) => Iter::Named(named.buffer.data.iter().enumerate()),
            Self::Slots {
                values,
                first,
                last,
            } => Iter::Slots {
                values: &values.data,
                first: *first,
                last: *last,
            },
        }
    }

    pub fn bindings(
        &self,
        ctx: &mut CallContext,
        class: &Namespace,
    ) -> Result<Buffer<(Value, Value)>> {
        match self {
            Self::Named(named) => {
                let mut values = Buffer::with_capacity(ctx, named.buffer.data.len())?;
                values.extend(ctx, &named.buffer.data)?;
                Ok(values)
            }
            Self::Slots { .. } => {
                let mut values = Buffer::with_capacity(ctx, self.iter().count())?;
                for (slot, value) in self.iter() {
                    let name = ctx.bytes(self.name(class, slot))?;
                    values.data.push((name, value.clone()));
                }
                Ok(values)
            }
        }
    }
}

pub(super) enum Iter<'a> {
    Named(std::iter::Enumerate<std::slice::Iter<'a, (Value, Value)>>),
    Slots {
        values: &'a [Slot],
        first: u32,
        last: u32,
    },
}

impl<'a> Iterator for Iter<'a> {
    type Item = (usize, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Named(iter) => iter.next().map(|(slot, (_, value))| (slot, value)),
            Self::Slots {
                values,
                first,
                last,
            } => {
                if *first == END {
                    return None;
                }
                let slot = *first as usize;
                if *first == *last {
                    *first = END;
                    *last = END;
                } else {
                    *first = values[slot].next;
                }
                Some((slot, &values[slot].value))
            }
        }
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Named(iter) => iter.next_back().map(|(slot, (_, value))| (slot, value)),
            Self::Slots {
                values,
                first,
                last,
            } => {
                if *last == END {
                    return None;
                }
                let slot = *last as usize;
                if *first == *last {
                    *first = END;
                    *last = END;
                } else {
                    *last = values[slot].previous;
                }
                Some((slot, &values[slot].value))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Engine};

    #[test]
    fn sparse_slot_initialization_observes_work_limits_before_publication() {
        let mut options = CallOptions::default();
        options.limits.steps = Some(3);
        let mut ctx = CallContext::new(options);
        let mut fields = Fields::Slots {
            values: Buffer::empty(),
            first: END,
            last: END,
        };
        let error = fields.set_slot(&mut ctx, 4096, Value::int(1)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert!(fields.get(4096).is_none());
        assert!(fields.iter().next().is_none());
        assert!(ctx.stats().retained_memory_bytes > 0);
        drop(fields);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn slots_preserve_assignment_order_and_missing_fields_across_imports() {
        let script = Engine::new()
            .compile(
                r#"
class Row
  @a: int
  @z: int
  property missing: int?
  def initialize(reverse: bool)
    if reverse
      @z = 3
      @a = 1
    else
      @a = 1
      @z = 3
    end
  end
end
def run(reverse: bool) -> Row
  Row.new(reverse)
end
"#,
            )
            .unwrap();
        for reverse in [false, true] {
            let result = script
                .call("run", &[Value::boolean(reverse)], CallOptions::default())
                .unwrap();
            let mut ctx = CallContext::new(CallOptions::default());
            let value = ctx.import(&result.value).unwrap();
            let Kind::Instance(instance) = &value.0 else {
                panic!("instance")
            };
            let layout = instance.class().field_layout().unwrap();
            let a = layout.iter().position(|name| name == "a").unwrap();
            let missing = layout.iter().position(|name| name == "missing").unwrap();
            assert_eq!(get_slot(&mut ctx, instance, a).unwrap().as_int(), Some(1));
            assert!(matches!(
                get_slot(&mut ctx, instance, missing).unwrap().0,
                Kind::Nil
            ));
            let names = |ctx: &mut CallContext| {
                bindings(ctx, instance)
                    .unwrap()
                    .data
                    .iter()
                    .map(|(key, _)| key.as_bytes().unwrap().to_vec())
                    .collect::<Vec<_>>()
            };
            let mut expected = if reverse {
                vec![b"z".to_vec(), b"a".to_vec()]
            } else {
                vec![b"a".to_vec(), b"z".to_vec()]
            };
            assert_eq!(names(&mut ctx), expected);
            set_slot(&mut ctx, instance, a, Value::int(10)).unwrap();
            assert_eq!(names(&mut ctx), expected);
            set_slot(&mut ctx, instance, missing, Value::nil()).unwrap();
            expected.push(b"missing".to_vec());
            assert_eq!(names(&mut ctx), expected);
            let heap = instance.heap().unwrap();
            let data = heap.data.lock().unwrap();
            let fields = &data.entries.data[instance.identity.slot.load(Ordering::Relaxed)].fields;
            let mut iter = fields.iter();
            assert_eq!(iter.next_back().unwrap().0, missing);
            assert!(iter.next().is_some());
            assert!(iter.next_back().is_some());
            assert!(iter.next().is_none());
            assert!(iter.next_back().is_none());
        }
    }
}
