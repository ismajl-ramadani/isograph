use common_lang_types::{
    InputTypeName, InputValueName, IsographObjectTypeName, ScalarFieldName, SelectableFieldName,
    UnvalidatedTypeName, VariableName, WithLocation, WithSpan,
};
use graphql_lang_types::{GraphQLInputValueDefinition, NamedTypeAnnotation, TypeAnnotation};
use isograph_lang_types::{
    ClientFieldId, LinkedFieldSelection, ObjectId, ScalarFieldSelection, ScalarId,
    SelectableFieldId, Selection, ServerFieldId, UnvalidatedScalarFieldSelection,
    UnvalidatedSelection, VariableDefinition,
};
use thiserror::Error;

use crate::{
    refetched_paths::refetched_paths_with_path, ClientField, FieldDefinitionLocation,
    NameAndArguments, PathToRefetchField, Schema, SchemaData, SchemaIdField, SchemaObject,
    SchemaServerField, SchemaValidationState, UnvalidatedClientField,
    UnvalidatedLinkedFieldSelection, UnvalidatedSchema, UnvalidatedSchemaData,
    UnvalidatedSchemaField, UnvalidatedSchemaObject, UnvalidatedSchemaServerField,
    ValidateEntrypointDeclarationError,
};

pub type ValidatedSchemaServerField = SchemaServerField<TypeAnnotation<SelectableFieldId>>;

pub type ValidatedSelection = Selection<
    <ValidatedSchemaState as SchemaValidationState>::ClientFieldSelectionScalarFieldAssociatedData,
    <ValidatedSchemaState as SchemaValidationState>::ClientFieldSelectionLinkedFieldAssociatedData,
>;

pub type ValidatedLinkedFieldSelection = LinkedFieldSelection<
    <ValidatedSchemaState as SchemaValidationState>::ClientFieldSelectionScalarFieldAssociatedData,
    <ValidatedSchemaState as SchemaValidationState>::ClientFieldSelectionLinkedFieldAssociatedData,
>;
pub type ValidatedScalarFieldSelection = ScalarFieldSelection<
    <ValidatedSchemaState as SchemaValidationState>::ClientFieldSelectionScalarFieldAssociatedData,
>;

pub type ValidatedVariableDefinition = VariableDefinition<SelectableFieldId>;
pub type ValidatedClientField = ClientField<
    <ValidatedSchemaState as SchemaValidationState>::ClientFieldSelectionScalarFieldAssociatedData,
    <ValidatedSchemaState as SchemaValidationState>::ClientFieldSelectionLinkedFieldAssociatedData,
    <ValidatedSchemaState as SchemaValidationState>::ClientFieldVariableDefinitionAssociatedData,
>;

/// The validated defined field that shows up in the TScalarField generic.
pub type ValidatedFieldDefinitionLocation = FieldDefinitionLocation<ServerFieldId, ClientFieldId>;

pub type ValidatedSchemaObject =
    SchemaObject<<ValidatedSchemaState as SchemaValidationState>::EncounteredField>;

pub type ValidatedSchemaIdField = SchemaIdField<NamedTypeAnnotation<ScalarId>>;

#[derive(Debug)]
pub struct ValidatedLinkedFieldAssociatedData {
    pub parent_object_id: ObjectId,
}

#[derive(Debug)]
pub struct ValidatedSchemaState {}
impl SchemaValidationState for ValidatedSchemaState {
    type FieldTypeAssociatedData = SelectableFieldId;
    type ClientFieldSelectionScalarFieldAssociatedData = ValidatedFieldDefinitionLocation;
    type ClientFieldSelectionLinkedFieldAssociatedData = ValidatedLinkedFieldAssociatedData;
    type ClientFieldVariableDefinitionAssociatedData = SelectableFieldId;
    type EncounteredField = ValidatedFieldDefinitionLocation;
    type Entrypoint = ClientFieldId;
}

pub type ValidatedSchema = Schema<ValidatedSchemaState>;

impl ValidatedSchema {
    pub fn validate_and_construct(
        unvalidated_schema: UnvalidatedSchema,
    ) -> Result<Self, Vec<WithLocation<ValidateSchemaError>>> {
        let mut errors = vec![];

        let mut updated_entrypoints = vec![];
        for (text_source, entrypoint_type_and_field) in unvalidated_schema.entrypoints.iter() {
            match unvalidated_schema
                .validate_entrypoint_type_and_field(*text_source, *entrypoint_type_and_field)
                .map_err(|e| {
                    WithLocation::new(
                        ValidateSchemaError::ErrorValidatingEntrypointDeclaration {
                            message: e.item,
                        },
                        e.location,
                    )
                }) {
                Ok(client_field_id) => updated_entrypoints.push(client_field_id),
                Err(e) => errors.push(e),
            }
        }

        let Schema {
            server_fields: fields,
            client_fields,
            entrypoints: _,
            schema_data,
            id_type_id: id_type,
            string_type_id: string_type,
            query_type_id,
            float_type_id,
            boolean_type_id,
            int_type_id,
        } = unvalidated_schema;

        let updated_fields = match validate_and_transform_fields(fields, &schema_data) {
            Ok(fields) => fields,
            Err(new_errors) => {
                errors.extend(new_errors);
                return Err(errors);

                // Because fields flows into updated_resolvers, we cannot optimistically
                // continue here.
                // TODO: figure out whether this can be worked around.
            }
        };

        let updated_client_fields = match validate_and_transform_client_fields(
            client_fields,
            &schema_data,
            &updated_fields,
        ) {
            Ok(client_fields) => client_fields,
            Err(new_errors) => {
                errors.extend(new_errors);
                vec![]
            }
        };

        let SchemaData {
            objects,
            scalars,
            defined_types,
        } = schema_data;

        if errors.is_empty() {
            let objects = objects
                .into_iter()
                .map(|object| {
                    transform_object_field_ids(&updated_fields, &updated_client_fields, object)
                })
                .collect();

            Ok(Self {
                server_fields: updated_fields,
                client_fields: updated_client_fields,
                entrypoints: updated_entrypoints,
                schema_data: SchemaData {
                    objects,
                    scalars,
                    defined_types,
                },
                id_type_id: id_type,
                string_type_id: string_type,
                query_type_id,
                float_type_id,
                boolean_type_id,
                int_type_id,
            })
        } else {
            Err(errors)
        }
    }
}

fn transform_object_field_ids(
    schema_fields: &[ValidatedSchemaServerField],
    validated_client_fields: &[ValidatedClientField],
    unvalidated_object: UnvalidatedSchemaObject,
) -> ValidatedSchemaObject {
    let SchemaObject {
        name,
        server_fields,
        description,
        id,
        encountered_fields: unvalidated_encountered_fields,
        client_field_ids,
        id_field,
        directives,
    } = unvalidated_object;

    let validated_encountered_fields = unvalidated_encountered_fields
        .into_iter()
        .map(|(encountered_field_name, _)| {
            for server_field_id in server_fields.iter() {
                let field = &schema_fields[server_field_id.as_usize()];
                if field.name.item == encountered_field_name {
                    return (
                        encountered_field_name,
                        FieldDefinitionLocation::Server(field.id),
                    );
                }
            }
            for client_field_id in client_field_ids.iter() {
                let client_field = &validated_client_fields[client_field_id.as_usize()];
                if client_field.name == encountered_field_name {
                    return (
                        encountered_field_name,
                        FieldDefinitionLocation::Client(client_field.id),
                    );
                }
            }
            panic!(
                "field {:?} not found, probably a isograph bug but we should confirm",
                encountered_field_name
            );
        })
        .collect();

    SchemaObject {
        description,
        name,
        id,
        server_fields,
        encountered_fields: validated_encountered_fields,
        client_field_ids,
        id_field,
        directives,
    }
}

fn validate_and_transform_fields(
    fields: Vec<UnvalidatedSchemaField>,
    schema_data: &UnvalidatedSchemaData,
) -> Result<Vec<ValidatedSchemaServerField>, Vec<WithLocation<ValidateSchemaError>>> {
    get_all_errors_or_all_ok_iter(
        fields
            .into_iter()
            .map(|field| validate_and_transform_field(field, schema_data)),
    )
}

fn get_all_errors_or_all_ok<T, E>(
    items: impl Iterator<Item = Result<T, E>>,
) -> Result<Vec<T>, Vec<E>> {
    let mut oks = vec![];
    let mut errors = vec![];

    for item in items {
        match item {
            Ok(ok) => oks.push(ok),
            Err(e) => errors.push(e),
        }
    }

    if errors.is_empty() {
        Ok(oks)
    } else {
        Err(errors)
    }
}

fn get_all_errors_or_all_ok_iter<T, E>(
    items: impl Iterator<Item = Result<T, impl Iterator<Item = E>>>,
) -> Result<Vec<T>, Vec<E>> {
    let mut oks = vec![];
    let mut errors = vec![];

    for item in items {
        match item {
            Ok(ok) => oks.push(ok),
            Err(e) => errors.extend(e),
        }
    }

    if errors.is_empty() {
        Ok(oks)
    } else {
        Err(errors)
    }
}

fn validate_and_transform_field(
    field: UnvalidatedSchemaField,
    schema_data: &UnvalidatedSchemaData,
) -> Result<ValidatedSchemaServerField, impl Iterator<Item = WithLocation<ValidateSchemaError>>> {
    // TODO rewrite as field.map(...).transpose()
    let (empty_field, server_field_type) = field.split();

    let mut errors = vec![];

    let field_type =
        match validate_server_field_type_exists(schema_data, &server_field_type, &empty_field) {
            Ok(type_annotation) => Some(type_annotation),
            Err(e) => {
                errors.push(e);
                None
            }
        };

    let valid_arguments = match {
        get_all_errors_or_all_ok(empty_field.arguments.into_iter().map(|arg| {
            validate_server_field_argument(
                arg,
                schema_data,
                empty_field.parent_type_id,
                empty_field.name,
            )
        }))
    } {
        Ok(arguments) => Some(arguments),
        Err(e) => {
            errors.extend(e);
            None
        }
    };

    if let Some(field_type) = field_type {
        if let Some(valid_arguments) = valid_arguments {
            return Ok(SchemaServerField {
                description: empty_field.description,
                name: empty_field.name,
                id: empty_field.id,
                associated_data: field_type,
                parent_type_id: empty_field.parent_type_id,
                arguments: valid_arguments,
            });
        }
    }

    Err(errors.into_iter())
}

fn validate_server_field_type_exists(
    schema_data: &UnvalidatedSchemaData,
    server_field_type: &TypeAnnotation<UnvalidatedTypeName>,
    field: &SchemaServerField<()>,
) -> ValidateSchemaResult<TypeAnnotation<SelectableFieldId>> {
    // look up the item in defined_types. If it's not there, error.
    match schema_data.defined_types.get(server_field_type.inner()) {
        // Why do we need to clone here? Can we avoid this?
        Some(type_id) => Ok(server_field_type.clone().map(|_| *type_id)),
        None => Err(WithLocation::new(
            ValidateSchemaError::FieldTypenameDoesNotExist {
                parent_type_name: schema_data.object(field.parent_type_id).name,
                field_name: field.name.item,
                field_type: *server_field_type.inner(),
            },
            field.name.location,
        )),
    }
}

fn validate_server_field_argument(
    argument: WithLocation<GraphQLInputValueDefinition>,
    schema_data: &UnvalidatedSchemaData,
    parent_type_id: ObjectId,
    name: WithLocation<SelectableFieldName>,
) -> ValidateSchemaResult<WithLocation<GraphQLInputValueDefinition>> {
    // Isograph doesn't care about the default value, and that remains
    // unvalidated.

    // look up the item in defined_types. If it's not there, error.
    match schema_data
        .defined_types
        .get(&(*argument.item.type_.inner()).into())
    {
        Some(_) => Ok(argument),
        None => Err(WithLocation::new(
            ValidateSchemaError::FieldArgumentTypeDoesNotExist {
                parent_type_name: schema_data.object(parent_type_id).name,
                field_name: name.item,
                argument_name: argument.item.name.item,
                argument_type: *argument.item.type_.inner(),
            },
            name.location,
        )),
    }
}

fn validate_and_transform_client_fields(
    client_fields: Vec<UnvalidatedClientField>,
    schema_data: &UnvalidatedSchemaData,
    server_fields: &[UnvalidatedSchemaServerField],
) -> Result<Vec<ValidatedClientField>, Vec<WithLocation<ValidateSchemaError>>> {
    get_all_errors_or_all_ok(client_fields.into_iter().map(|client_field| {
        validate_client_field_selection_set(schema_data, client_field, server_fields)
    }))
}

fn validate_client_field_selection_set(
    schema_data: &UnvalidatedSchemaData,
    unvalidated_client_field: UnvalidatedClientField,
    server_fields: &[UnvalidatedSchemaServerField],
) -> ValidateSchemaResult<ValidatedClientField> {
    let variable_definitions =
        validate_variable_definitions(schema_data, unvalidated_client_field.variable_definitions)?;

    match unvalidated_client_field.selection_set_and_unwraps {
        Some((selection_set, unwraps)) => {
            let parent_object = schema_data.object(unvalidated_client_field.parent_object_id);
            let selection_set = validate_client_field_definition_selections_exist_and_types_match(
                schema_data,
                selection_set,
                parent_object,
                server_fields,
            )
            .map_err(|err| {
                validate_selections_error_to_validate_schema_error(
                    err,
                    parent_object,
                    unvalidated_client_field.name,
                )
            })?;
            Ok(ClientField {
                description: unvalidated_client_field.description,
                name: unvalidated_client_field.name,
                id: unvalidated_client_field.id,
                selection_set_and_unwraps: Some((selection_set, unwraps)),
                variant: unvalidated_client_field.variant,
                variable_definitions,
                type_and_field: unvalidated_client_field.type_and_field,
                parent_object_id: unvalidated_client_field.parent_object_id,
                action_kind: unvalidated_client_field.action_kind,
            })
        }
        None => Ok(ClientField {
            description: unvalidated_client_field.description,
            name: unvalidated_client_field.name,
            id: unvalidated_client_field.id,
            selection_set_and_unwraps: None,
            variant: unvalidated_client_field.variant,
            variable_definitions,
            type_and_field: unvalidated_client_field.type_and_field,
            parent_object_id: unvalidated_client_field.parent_object_id,
            action_kind: unvalidated_client_field.action_kind,
        }),
    }
}

fn validate_variable_definitions(
    schema_data: &UnvalidatedSchemaData,
    variable_definitions: Vec<WithSpan<VariableDefinition<UnvalidatedTypeName>>>,
) -> ValidateSchemaResult<Vec<WithSpan<ValidatedVariableDefinition>>> {
    variable_definitions
        .into_iter()
        .map(|x| {
            x.and_then(|vd| {
                // TODO this should be doable in the error branch
                let type_string = vd.type_.to_string();
                let inner_type = *vd.type_.inner();
                Ok(VariableDefinition {
                    name: vd.name,
                    type_: vd.type_.and_then(|type_name| {
                        match schema_data.defined_types.get(&type_name) {
                            Some(type_id) => Ok(*type_id),
                            None => Err(WithLocation::new(
                                ValidateSchemaError::VariableDefinitionInnerTypeDoesNotExist {
                                    variable_name: vd.name.item,
                                    type_: type_string,
                                    inner_type,
                                },
                                vd.name.location,
                            )),
                        }
                    })?,
                })
            })
        })
        .collect()
}

fn validate_selections_error_to_validate_schema_error(
    err: WithLocation<ValidateSelectionsError>,
    parent_object: &UnvalidatedSchemaObject,
    client_field_name: SelectableFieldName,
) -> WithLocation<ValidateSchemaError> {
    err.map(|item| match item {
        ValidateSelectionsError::FieldDoesNotExist(field_parent_type_name, field_name) => {
            ValidateSchemaError::ClientFieldSelectionFieldDoesNotExist {
                client_field_parent_type_name: parent_object.name,
                client_field_name,
                field_parent_type_name,
                field_name,
            }
        }
        ValidateSelectionsError::FieldSelectedAsScalarButTypeIsNotScalar {
            field_parent_type_name: parent_type_name,
            field_name,
            target_type,
            target_type_name,
        } => ValidateSchemaError::ClientFieldSelectionFieldIsNotScalar {
            client_field_parent_type_name: parent_object.name,
            client_field_name,
            field_parent_type_name: parent_type_name,
            field_name,
            field_type: target_type,
            target_type_name,
        },
        ValidateSelectionsError::FieldSelectedAsLinkedButTypeIsScalar {
            field_parent_type_name,
            field_name,
            target_type,
            target_type_name,
        } => ValidateSchemaError::ClientFieldSelectionFieldIsScalar {
            client_field_parent_type_name: parent_object.name,
            client_field_name,
            field_parent_type_name,
            field_name,
            field_type: target_type,
            target_type_name,
        },
        ValidateSelectionsError::FieldSelectedAsLinkedButTypeIsResolver {
            field_parent_type_name,
            field_name,
        } => ValidateSchemaError::ClientFieldSelectionFieldIsResolver {
            client_field_parent_type_name: parent_object.name,
            client_field_name: field_name,
            field_parent_type_name,
            field_name,
        },
    })
}

type ValidateSelectionsResult<T> = Result<T, WithLocation<ValidateSelectionsError>>;

#[allow(unused)]
#[derive(Debug)]
enum ValidateSelectionsError {
    FieldDoesNotExist(IsographObjectTypeName, SelectableFieldName),
    FieldSelectedAsScalarButTypeIsNotScalar {
        field_parent_type_name: IsographObjectTypeName,
        field_name: SelectableFieldName,
        target_type: &'static str,
        target_type_name: UnvalidatedTypeName,
    },
    FieldSelectedAsLinkedButTypeIsScalar {
        field_parent_type_name: IsographObjectTypeName,
        field_name: SelectableFieldName,
        target_type: &'static str,
        target_type_name: UnvalidatedTypeName,
    },
    FieldSelectedAsLinkedButTypeIsResolver {
        field_parent_type_name: IsographObjectTypeName,
        field_name: SelectableFieldName,
    },
}

fn validate_client_field_definition_selections_exist_and_types_match(
    schema_data: &UnvalidatedSchemaData,
    selection_set: Vec<WithSpan<UnvalidatedSelection>>,
    parent_object: &UnvalidatedSchemaObject,
    server_fields: &[UnvalidatedSchemaServerField],
) -> ValidateSelectionsResult<Vec<WithSpan<ValidatedSelection>>> {
    // Currently, we only check that each field exists and has an appropriate type, not that
    // there are no selection conflicts due to aliases or parameters.

    Ok(selection_set
        .into_iter()
        .map(|selection| {
            validate_client_field_definition_selection_exists_and_type_matches(
                selection,
                parent_object,
                schema_data,
                server_fields,
            )
        })
        .collect::<Result<_, _>>()?)
}

fn validate_client_field_definition_selection_exists_and_type_matches(
    selection: WithSpan<UnvalidatedSelection>,
    parent_object: &UnvalidatedSchemaObject,
    schema_data: &UnvalidatedSchemaData,
    server_fields: &[UnvalidatedSchemaServerField],
) -> ValidateSelectionsResult<WithSpan<ValidatedSelection>> {
    selection.and_then(|selection| {
        selection.and_then(&mut |field_selection| {
            field_selection.and_then(
                &mut |scalar_field_selection| {
                    validate_field_type_exists_and_is_scalar(
                        schema_data,
                        parent_object,
                        scalar_field_selection,
                        server_fields,
                    )
                },
                &mut |linked_field_selection| {
                    validate_field_type_exists_and_is_linked(
                        schema_data,
                        parent_object,
                        linked_field_selection,
                        server_fields,
                    )
                },
            )
        })
    })
}

/// Given that we selected a scalar field, the field should exist on the parent,
/// and type should be a resolver (which is a scalar) or a server scalar type.
fn validate_field_type_exists_and_is_scalar(
    schema_data: &UnvalidatedSchemaData,
    parent_object: &UnvalidatedSchemaObject,
    scalar_field_selection: UnvalidatedScalarFieldSelection,
    server_fields: &[UnvalidatedSchemaServerField],
) -> ValidateSelectionsResult<ValidatedScalarFieldSelection> {
    let scalar_field_name = scalar_field_selection.name.item.into();
    match parent_object.encountered_fields.get(&scalar_field_name) {
        Some(defined_field_type) => match defined_field_type {
            FieldDefinitionLocation::Server(server_field_name) => {
                let field_type_id = *schema_data
                    .defined_types
                    .get(server_field_name.inner())
                    .expect(
                        "Expected field type to be defined, which I think \
                        was validated earlier, probably indicates a bug in Isograph",
                    );
                match field_type_id {
                    SelectableFieldId::Scalar(_scalar_id) => Ok(ScalarFieldSelection {
                        name: scalar_field_selection.name,
                        associated_data: FieldDefinitionLocation::Server(
                            find_server_field_id(
                                server_fields,
                                scalar_field_selection.name.item,
                                &parent_object.server_fields,
                            )
                            .expect("Expected to find scalar field, this probably indicates a bug in Isograph"),
                        ),
                        reader_alias: scalar_field_selection.reader_alias,
                        normalization_alias: scalar_field_selection.normalization_alias,
                        unwraps: scalar_field_selection.unwraps,
                        arguments: scalar_field_selection.arguments,
                    }),
                    SelectableFieldId::Object(_) => Err(
                        WithLocation::new(
                            ValidateSelectionsError::FieldSelectedAsScalarButTypeIsNotScalar {
                                field_parent_type_name: parent_object.name,
                                field_name: scalar_field_name,
                                target_type: "an object",
                                target_type_name: *server_field_name.inner(),
                            },
                            scalar_field_selection.name.location
                        ),
                    ),
                }
            }
            FieldDefinitionLocation::Client(client_field_id) => {
                // TODO confirm this works if resolver_name is an alias
                Ok(ScalarFieldSelection {
                    name: scalar_field_selection.name,
                    reader_alias: scalar_field_selection.reader_alias,
                    unwraps: scalar_field_selection.unwraps,
                    associated_data: FieldDefinitionLocation::Client(*client_field_id),
                    arguments: scalar_field_selection.arguments,
                    normalization_alias: scalar_field_selection.normalization_alias,
                })
            }
        },
        None => Err(WithLocation::new(
            ValidateSelectionsError::FieldDoesNotExist(parent_object.name, scalar_field_name),
            scalar_field_selection.name.location,
        )),
    }
}

/// Given that we selected a linked field, the field should exist on the parent,
/// and type should be a server interface, object or union.
fn validate_field_type_exists_and_is_linked(
    schema_data: &UnvalidatedSchemaData,
    parent_object: &UnvalidatedSchemaObject,
    linked_field_selection: UnvalidatedLinkedFieldSelection,
    server_fields: &[UnvalidatedSchemaServerField],
) -> ValidateSelectionsResult<ValidatedLinkedFieldSelection> {
    let linked_field_name = linked_field_selection.name.item.into();
    match (&parent_object.encountered_fields).get(&linked_field_name) {
        Some(defined_field_type) => match defined_field_type {
            FieldDefinitionLocation::Server(server_field_name) => {
                let field_type_id = *schema_data
                    .defined_types
                    .get(server_field_name.inner())
                    .expect(
                        "Expected field type to be defined, which I \
                            think was validated earlier, probably indicates a bug in Isograph",
                    );
                match field_type_id {
                    SelectableFieldId::Scalar(_) => Err(WithLocation::new(
                        ValidateSelectionsError::FieldSelectedAsLinkedButTypeIsScalar {
                            field_parent_type_name: parent_object.name,
                            field_name: linked_field_name,
                            target_type: "a scalar",
                            target_type_name: *server_field_name.inner(),
                        },
                        linked_field_selection.name.location,
                    )),
                    SelectableFieldId::Object(object_id) => {
                        let object = schema_data.objects.get(object_id.as_usize()).unwrap();
                        Ok(LinkedFieldSelection {
                                name: linked_field_selection.name,
                                reader_alias: linked_field_selection.reader_alias,
                                normalization_alias: linked_field_selection.normalization_alias,
                                selection_set: linked_field_selection.selection_set.into_iter().map(
                                    |selection| {
                                        validate_client_field_definition_selection_exists_and_type_matches(
                                            selection,
                                            object,
                                            schema_data,
                                            server_fields
                                        )
                                    },
                                ).collect::<Result<Vec<_>, _>>()?,
                                unwraps: linked_field_selection.unwraps,
                                associated_data: ValidatedLinkedFieldAssociatedData {
                                    parent_object_id: object_id,
                                },
                                arguments: linked_field_selection.arguments,
                            })
                    }
                }
            }
            FieldDefinitionLocation::Client(_) => Err(WithLocation::new(
                ValidateSelectionsError::FieldSelectedAsLinkedButTypeIsResolver {
                    field_parent_type_name: parent_object.name,
                    field_name: linked_field_name,
                },
                linked_field_selection.name.location,
            )),
        },
        None => Err(WithLocation::new(
            ValidateSelectionsError::FieldDoesNotExist(parent_object.name, linked_field_name),
            linked_field_selection.name.location,
        )),
    }
}

fn find_server_field_id(
    server_fields: &[UnvalidatedSchemaServerField],
    field_name: ScalarFieldName,
    parent_server_fields: &[ServerFieldId],
) -> Option<ServerFieldId> {
    parent_server_fields.iter().find_map(|server_field_id| {
        let server_field = &server_fields[server_field_id.as_usize()];
        if server_field.name.item == field_name.into() {
            Some(*server_field_id)
        } else {
            None
        }
    })
}

type ValidateSchemaResult<T> = Result<T, WithLocation<ValidateSchemaError>>;

#[derive(Debug, Error)]
pub enum ValidateSchemaError {
    #[error(
        "The field `{parent_type_name}.{field_name}` has inner type `{field_type}`, which does not exist."
    )]
    FieldTypenameDoesNotExist {
        parent_type_name: IsographObjectTypeName,
        field_name: SelectableFieldName,
        field_type: UnvalidatedTypeName,
    },

    #[error(
        "The argument `{argument_name}` on field `{parent_type_name}.{field_name}` has inner type `{argument_type}`, which does not exist."
    )]
    FieldArgumentTypeDoesNotExist {
        argument_name: InputValueName,
        parent_type_name: IsographObjectTypeName,
        field_name: SelectableFieldName,
        argument_type: InputTypeName,
    },

    #[error(
        "In the client field `{client_field_parent_type_name}.{client_field_name}`, \
        the field `{field_parent_type_name}.{field_name}` is selected, but that \
        field does not exist on `{field_parent_type_name}`"
    )]
    ClientFieldSelectionFieldDoesNotExist {
        client_field_parent_type_name: IsographObjectTypeName,
        client_field_name: SelectableFieldName,
        field_parent_type_name: IsographObjectTypeName,
        field_name: SelectableFieldName,
    },

    #[error(
        "In the client field `{client_field_parent_type_name}.{client_field_name}`, \
        the field `{field_parent_type_name}.{field_name}` is selected as a scalar, \
        but that field's type is `{target_type_name}`, which is {field_type}."
    )]
    ClientFieldSelectionFieldIsNotScalar {
        client_field_parent_type_name: IsographObjectTypeName,
        client_field_name: SelectableFieldName,
        field_parent_type_name: IsographObjectTypeName,
        field_name: SelectableFieldName,
        field_type: &'static str,
        target_type_name: UnvalidatedTypeName,
    },

    #[error(
        "In the client field `{client_field_parent_type_name}.{client_field_name}`, \
        the field `{field_parent_type_name}.{field_name}` is selected as a linked field, \
        but that field's type is `{target_type_name}`, which is {field_type}."
    )]
    ClientFieldSelectionFieldIsScalar {
        client_field_parent_type_name: IsographObjectTypeName,
        client_field_name: SelectableFieldName,
        field_parent_type_name: IsographObjectTypeName,
        field_name: SelectableFieldName,
        field_type: &'static str,
        target_type_name: UnvalidatedTypeName,
    },

    #[error(
        "In the client field `{client_field_parent_type_name}.{client_field_name}`, the \
        field `{field_parent_type_name}.{field_name}` is selected as a linked field, \
        but that field is a client field, which can only be selected as a scalar."
    )]
    ClientFieldSelectionFieldIsResolver {
        client_field_parent_type_name: IsographObjectTypeName,
        client_field_name: SelectableFieldName,
        field_parent_type_name: IsographObjectTypeName,
        field_name: SelectableFieldName,
    },

    #[error(
        "The variable `{variable_name}` has type `{type_}`, but the inner type \
        `{inner_type}` does not exist."
    )]
    VariableDefinitionInnerTypeDoesNotExist {
        variable_name: VariableName,
        type_: String,
        inner_type: UnvalidatedTypeName,
    },

    #[error("Error when validating iso entrypoint calls.\nMessage: {message}")]
    ErrorValidatingEntrypointDeclaration {
        message: ValidateEntrypointDeclarationError,
    },
}

pub fn refetched_paths_for_client_field(
    validated_client_field: &ValidatedClientField,
    schema: &ValidatedSchema,
    path: &mut Vec<NameAndArguments>,
) -> Vec<PathToRefetchField> {
    let path_set = match &validated_client_field.selection_set_and_unwraps {
        Some((selection_set, _)) => refetched_paths_with_path(&selection_set, schema, path),
        None => panic!("unexpected non-existent selection set on client field"),
    };
    let mut paths: Vec<_> = path_set.into_iter().collect();
    paths.sort();
    paths
}
