use anyhow::Result;

use crate::spec::*;

mod spec;

const MAX_LINE_LENGTH: usize = 100;

const STARKNET_API_OPENRPC: &str = include_str!("./specs/0.2.1/starknet_api_openrpc.json");

struct RustType {
    title: Option<String>,
    description: Option<String>,
    name: String,
    content: RustTypeKind,
}

enum RustTypeKind {
    Struct(RustStruct),
    Enum(RustEnum),
    Wrapper(RustWrapper),
}

struct RustStruct {
    fields: Vec<RustField>,
}

struct RustEnum {
    variants: Vec<RustVariant>,
}

struct RustWrapper {
    type_name: String,
}

struct RustField {
    description: Option<String>,
    name: String,
    type_name: String,
    serde_as: Option<String>,
}

struct RustVariant {
    description: Option<String>,
    name: String,
    serde_name: String,
}

struct RustFieldType {
    type_name: String,
    serde_as: Option<String>,
}

impl RustType {
    pub fn render_stdout(&self, trailing_line: bool) {
        match (self.title.as_ref(), self.description.as_ref()) {
            (Some(title), Some(description)) => {
                print_doc(title, 0);
                println!("///");
                print_doc(description, 0);
            }
            (Some(title), None) => {
                print_doc(title, 0);
            }
            (None, Some(description)) => {
                print_doc(description, 0);
            }
            (None, None) => {}
        }

        self.content.render_stdout(&self.name);

        if trailing_line {
            println!();
        }
    }
}

impl RustTypeKind {
    pub fn render_stdout(&self, name: &str) {
        match self {
            Self::Struct(value) => value.render_stdout(name),
            Self::Enum(value) => value.render_stdout(name),
            Self::Wrapper(value) => value.render_stdout(name),
        }
    }
}

impl RustStruct {
    pub fn render_stdout(&self, name: &str) {
        if self.fields.iter().any(|item| item.serde_as.is_some()) {
            println!("#[serde_as]");
        }
        println!("#[derive(Debug, Clone, Serialize, Deserialize)]");
        println!("pub struct {} {{", name);

        for field in self.fields.iter() {
            if let Some(doc) = &field.description {
                print_doc(doc, 4);
            }
            if let Some(serde_as) = &field.serde_as {
                println!("    #[serde_as(as = \"{}\")]", serde_as);
            }

            let escaped_name = if field.name == "type" {
                "r#type"
            } else {
                &field.name
            };
            println!("    pub {}: {},", escaped_name, field.type_name);
        }

        println!("}}");
    }
}

impl RustEnum {
    pub fn render_stdout(&self, name: &str) {
        println!("#[derive(Debug, Clone, Serialize, Deserialize)]");
        println!("pub enum {} {{", name);

        for variant in self.variants.iter() {
            if let Some(doc) = &variant.description {
                print_doc(doc, 4);
            }

            println!("    #[serde(rename = \"{}\")]", variant.serde_name);
            println!("    {},", variant.name);
        }

        println!("}}");
    }
}

impl RustWrapper {
    pub fn render_stdout(&self, name: &str) {
        println!("#[derive(Debug, Clone, Serialize, Deserialize)]");
        println!("pub struct {}(pub {});", name, self.type_name);
    }
}

fn main() {
    let specs: Specification =
        serde_json::from_str(STARKNET_API_OPENRPC).expect("Failed to parse specification");

    println!("use serde::{{Deserialize, Serialize}};");
    println!("use serde_with::serde_as;");
    println!("use starknet_core::{{");
    println!("    serde::{{byte_array::base64, unsigned_field_element::UfeHex}},");
    println!("    types::{{FieldElement, L1Address as EthAddress}},");
    println!("}};");
    println!();
    println!("use super::serde_impls::NumAsHex;");
    println!();

    let types = resolve_types(&specs).expect("Failed to resolve types");
    for (ind, rust_type) in types.iter().enumerate() {
        rust_type.render_stdout(ind != types.len() - 1);
    }
}

fn resolve_types(specs: &Specification) -> Result<Vec<RustType>> {
    let mut types = vec![];

    for (name, entity) in specs.components.schemas.iter() {
        let rusty_name = to_starknet_rs_name(name);

        let title = entity.title();
        let description = match entity.description() {
            Some(description) => Some(description),
            None => entity.summary(),
        };

        eprintln!("Processing schema: {}", name);

        // Manual override exists
        if get_field_type_override(name).is_some() {
            continue;
        }

        let content = {
            match entity {
                Schema::Ref(_) => RustTypeKind::Wrapper(RustWrapper {
                    type_name: get_rust_type_for_field(entity, specs)?.type_name,
                }),
                Schema::OneOf(_) => {
                    // TODO: implement
                    eprintln!("WARNING: enum generation with oneOf not implemented");
                    continue;
                }
                Schema::AllOf(_) | Schema::Primitive(Primitive::Object(_)) => {
                    let mut fields = vec![];
                    if flatten_schema_fields(entity, specs, &mut fields).is_err() {
                        eprintln!("WARNING: unable to generate struct for {name}");
                        continue;
                    }
                    RustTypeKind::Struct(RustStruct { fields })
                }
                Schema::Primitive(Primitive::String(value)) => match &value.r#enum {
                    Some(variants) => RustTypeKind::Enum(RustEnum {
                        variants: variants
                            .iter()
                            .map(|item| RustVariant {
                                description: None,
                                name: to_starknet_rs_name(item),
                                serde_name: item.to_owned(),
                            })
                            .collect(),
                    }),
                    None => {
                        anyhow::bail!(
                            "Unexpected non-enum string type when generating struct/enum"
                        );
                    }
                },
                _ => {
                    anyhow::bail!("Unexpected schema type when generating struct/enum");
                }
            }
        };

        types.push(RustType {
            title: title.map(|value| to_starknet_rs_doc(value, true)),
            description: description.map(|value| to_starknet_rs_doc(value, true)),
            name: rusty_name,
            content,
        });
    }

    Ok(types)
}

fn flatten_schema_fields(
    schema: &Schema,
    specs: &Specification,
    fields: &mut Vec<RustField>,
) -> Result<()> {
    match schema {
        Schema::Ref(value) => {
            let ref_type_name = value.name();
            let ref_type = match specs.components.schemas.get(ref_type_name) {
                Some(ref_type) => ref_type,
                None => anyhow::bail!("Ref target type not found: {}", ref_type_name),
            };

            // Schema redirection
            flatten_schema_fields(ref_type, specs, fields)?;
        }
        Schema::AllOf(value) => {
            // Recursively resolves types
            for item in value.all_of.iter() {
                flatten_schema_fields(item, specs, fields)?;
            }
        }
        Schema::Primitive(Primitive::Object(value)) => {
            for (name, prop_value) in value.properties.iter() {
                // For fields we keep things simple and only use one line
                let doc_string = match prop_value.title() {
                    Some(text) => Some(text),
                    None => match prop_value.description() {
                        Some(text) => Some(text),
                        None => prop_value.summary(),
                    },
                };

                let field_type = get_rust_type_for_field(prop_value, specs)?;

                fields.push(RustField {
                    description: doc_string.map(|value| to_starknet_rs_doc(value, false)),
                    name: name.to_owned(),
                    type_name: field_type.type_name,
                    serde_as: field_type.serde_as,
                });
            }
        }
        _ => {
            dbg!(schema);
            anyhow::bail!("Unexpected schema type when getting object fields");
        }
    }

    Ok(())
}

fn get_rust_type_for_field(schema: &Schema, specs: &Specification) -> Result<RustFieldType> {
    match schema {
        Schema::Ref(value) => {
            let ref_type_name = value.name();
            if !specs.components.schemas.contains_key(ref_type_name) {
                anyhow::bail!("Ref target type not found: {}", ref_type_name);
            }

            // Hard-coded special rules
            Ok(
                get_field_type_override(ref_type_name).unwrap_or_else(|| RustFieldType {
                    type_name: to_starknet_rs_name(ref_type_name),
                    serde_as: None,
                }),
            )
        }
        Schema::OneOf(_) => {
            anyhow::bail!("Anonymous oneOf types should not be used for properties");
        }
        Schema::AllOf(_) => {
            anyhow::bail!("Anonymous allOf types should not be used for properties");
        }
        Schema::Primitive(value) => match value {
            Primitive::Array(value) => {
                let item_type = get_rust_type_for_field(&value.items, specs)?;
                Ok(RustFieldType {
                    type_name: format!("Vec<{}>", item_type.type_name),
                    serde_as: item_type
                        .serde_as
                        .map(|serde_as| format!("Vec<{}>", serde_as)),
                })
            }
            Primitive::Boolean(_) => Ok(RustFieldType {
                type_name: String::from("bool"),
                serde_as: None,
            }),
            Primitive::Integer(_) => Ok(RustFieldType {
                type_name: String::from("u64"),
                serde_as: None,
            }),
            Primitive::Object(_) => {
                anyhow::bail!("Anonymous object types should not be used for properties");
            }
            Primitive::String(_) => Ok(RustFieldType {
                type_name: String::from("String"),
                serde_as: None,
            }),
        },
    }
}

fn get_field_type_override(type_name: &str) -> Option<RustFieldType> {
    Some(match type_name {
        "ADDRESS" | "STORAGE_KEY" | "TXN_HASH" | "FELT" | "BLOCK_HASH" | "CHAIN_ID"
        | "PROTOCOL_VERSION" => RustFieldType {
            type_name: String::from("FieldElement"),
            serde_as: Some(String::from("UfeHex")),
        },
        "BLOCK_NUMBER" => RustFieldType {
            type_name: String::from("u64"),
            serde_as: None,
        },
        "NUM_AS_HEX" => RustFieldType {
            type_name: String::from("u64"),
            serde_as: Some(String::from("NumAsHex")),
        },
        "ETH_ADDRESS" => RustFieldType {
            type_name: String::from("EthAddress"),
            serde_as: None,
        },
        "SIGNATURE" => RustFieldType {
            type_name: String::from("Vec<FieldElement>"),
            serde_as: Some(String::from("Vec<UfeHex>")),
        },
        "CONTRACT_ABI" => RustFieldType {
            type_name: String::from("Vec<ContractAbiEntry>"),
            serde_as: None,
        },
        "CONTRACT_ENTRY_POINT_LIST" => RustFieldType {
            type_name: String::from("Vec<ContractEntryPoint>"),
            serde_as: None,
        },
        _ => return None,
    })
}

fn print_doc(doc: &str, indent_spaces: usize) {
    let prefix = format!("{}/// ", " ".repeat(indent_spaces));
    for line in wrap_lines(doc, prefix.len()) {
        println!("{}{}", prefix, line);
    }
}

fn wrap_lines(doc: &str, prefix_length: usize) -> Vec<String> {
    let mut lines = vec![];
    let mut current_line = String::new();

    for part in doc.split(' ') {
        let mut addition = String::new();
        if !current_line.is_empty() {
            addition.push(' ');
        }
        addition.push_str(part);

        if prefix_length + current_line.len() + addition.len() <= MAX_LINE_LENGTH {
            current_line.push_str(&addition);
        } else {
            lines.push(current_line.clone());
            current_line.clear();
            current_line.push_str(part);
        }
    }

    lines.push(current_line);
    lines
}

fn to_starknet_rs_name(name: &str) -> String {
    to_pascal_case(name).replace("Txn", "Transaction")
}

fn to_starknet_rs_doc(doc: &str, force_period: bool) -> String {
    let mut doc = to_sentence_case(doc)
        .replace("starknet", "StarkNet")
        .replace("Starknet", "StarkNet")
        .replace("StarkNet.io", "starknet.io");

    if force_period && !doc.ends_with('.') {
        doc.push('.');
    }

    doc
}

fn to_pascal_case(name: &str) -> String {
    let mut result = String::new();

    let mut last_underscore = None;
    for (ind, character) in name.chars().enumerate() {
        if character == '_' {
            last_underscore = Some(ind);
            continue;
        }

        let uppercase = match last_underscore {
            Some(last_underscore) => ind == last_underscore + 1,
            None => ind == 0,
        };

        result.push(if uppercase {
            character.to_ascii_uppercase()
        } else {
            character.to_ascii_lowercase()
        });
    }

    result
}

fn to_sentence_case(name: &str) -> String {
    let mut result = String::new();

    let mut last_period = None;
    let mut last_char = None;

    for (ind, character) in name.chars().enumerate() {
        if character == '.' {
            last_period = Some(ind);
        }

        let uppercase = match last_period {
            Some(last_period) => ind == last_period + 2 && matches!(last_char, Some(' ')),
            None => ind == 0,
        };

        result.push(if uppercase {
            character.to_ascii_uppercase()
        } else {
            character.to_ascii_lowercase()
        });

        last_char = Some(character);
    }

    result
}
