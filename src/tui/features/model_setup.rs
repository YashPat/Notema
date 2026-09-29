//! Model setup: a stack of screens opened from the settings Model row.
//!
//! The landing screen is inferred from the saved base URL. Esc on the landing
//! screen returns to settings without writing. A deeper Esc pops one screen.
//! Saving writes `[model]` and returns to the Model row.

use notema_model::{OLLAMA_BASE_URL, PROVIDERS};

use crate::config::ModelSection;
use crate::tui::{
    app::AppModel,
    ollama_tags::OllamaTagsRequest,
    state::{ListNav, Overlay, SelectableList},
    text_input::TextInput,
    ui::interaction::TextFieldId,
};

pub(crate) const ASKING_OLLAMA: &str = "Asking Ollama for its models.";
const OLLAMA_EMPTY_NOTE: &str = "Ollama returned nothing.";
const DIFFERENT_SETUP: &str = "Different setup…";

/// One screen in the setup stack. Index 0 is the landing screen; [`ModelScreen`]
/// at the end of the stack is what is drawn.
pub(crate) enum ModelScreen {
    /// Local, Cloud, Other server.
    Choice,
    /// Ollama model names. `saved` is merged into a fetch when the stored URL
    /// is Ollama's.
    Local {
        names: Vec<String>,
        saved: Option<String>,
    },
    /// OpenAI, Groq, OpenRouter.
    Providers,
    Cloud {
        provider: usize,
        key: TextInput,
        other: TextInput,
    },
    Fields {
        base_url: TextInput,
        model: TextInput,
        token: TextInput,
        note: Option<&'static str>,
    },
}

/// What Enter does on the highlighted row.
enum Activation {
    Push {
        screen: ModelScreen,
        select: usize,
        fetch: bool,
    },
    Save {
        base_url: String,
        model: String,
        token: Option<String>,
    },
    Ignore,
}

pub(crate) struct ModelSetupState {
    pub(crate) stack: Vec<ModelScreen>,
    pub(crate) list: SelectableList,
    /// The in-flight Ollama tag request, if Local is waiting on one.
    pub(crate) pending_tags_id: Option<u64>,
}

impl ModelSetupState {
    pub(crate) fn from_section(section: &ModelSection) -> Self {
        let (screen, select) = landing(section);
        let mut state = Self {
            stack: vec![screen],
            list: SelectableList::default(),
            pending_tags_id: None,
        };
        state.select_index(select);
        state
    }

    pub(crate) fn current(&self) -> Option<&ModelScreen> {
        self.stack.last()
    }

    pub(crate) fn on_local(&self) -> bool {
        matches!(self.current(), Some(ModelScreen::Local { .. }))
    }

    pub(crate) fn title(&self) -> &'static str {
        match self.current() {
            Some(ModelScreen::Choice) | None => "Model",
            Some(ModelScreen::Local { .. }) => "Local",
            Some(ModelScreen::Providers) => "Cloud",
            Some(ModelScreen::Cloud { provider, .. }) => {
                PROVIDERS.get(*provider).map(|p| p.name).unwrap_or("Cloud")
            }
            Some(ModelScreen::Fields { .. }) => "Other server",
        }
    }

    /// True while Local has been asked for names and none have arrived.
    pub(crate) fn waiting_on_ollama(&self) -> bool {
        self.pending_tags_id.is_some()
            && matches!(
                self.current(),
                Some(ModelScreen::Local { names, .. }) if names.is_empty()
            )
    }

    pub(crate) fn row_label(&self, index: usize) -> String {
        self.current()
            .map(|screen| screen.label(index))
            .unwrap_or_default()
    }

    pub(crate) fn row_description(&self, index: usize) -> String {
        if self.waiting_on_ollama() {
            return ASKING_OLLAMA.to_string();
        }
        self.current()
            .map(|screen| screen.description(index))
            .unwrap_or_default()
    }

    pub(crate) fn field_id_at(&self, index: usize) -> Option<TextFieldId> {
        self.current().and_then(|screen| screen.field_id(index))
    }

    pub(crate) fn highlighted_field_id(&self) -> Option<TextFieldId> {
        self.field_id_at(self.selected_index()?)
    }

    pub(crate) fn field_input_mut(&mut self, index: usize) -> Option<&mut TextInput> {
        self.stack.last_mut()?.field_mut(index)
    }

    pub(crate) fn highlighted_input_mut(&mut self) -> Option<&mut TextInput> {
        let index = self.list.selected()?;
        self.stack.last_mut()?.field_mut(index)
    }

    pub(crate) fn input_for_mut(&mut self, id: TextFieldId) -> Option<&mut TextInput> {
        let index = self.row_for_field(id)?;
        self.stack.last_mut()?.field_mut(index)
    }

    pub(crate) fn row_for_field(&self, id: TextFieldId) -> Option<usize> {
        let screen = self.current()?;
        (0..screen.row_count()).find(|&index| screen.field_id(index) == Some(id))
    }

    fn reset_highlight(&mut self, index: usize) {
        self.list.select_none();
        self.list.set_offset(0);
        self.select_index(index);
    }

    fn activation(&self, index: usize, section: &ModelSection) -> Activation {
        let Some(screen) = self.current() else {
            return Activation::Ignore;
        };
        match screen.row(index) {
            Some(Row::Local) => Activation::Push {
                screen: ModelScreen::Local {
                    names: Vec::new(),
                    saved: ollama_saved(section),
                },
                select: 0,
                fetch: true,
            },
            Some(Row::Cloud) => Activation::Push {
                screen: ModelScreen::Providers,
                select: 0,
                fetch: false,
            },
            Some(Row::OtherServer) => Activation::Push {
                screen: empty_fields(),
                select: 0,
                fetch: false,
            },
            Some(Row::Name) => match screen {
                ModelScreen::Local { names, .. } => {
                    names
                        .get(index)
                        .map_or(Activation::Ignore, |name| Activation::Save {
                            base_url: OLLAMA_BASE_URL.to_string(),
                            model: name.clone(),
                            token: None,
                        })
                }
                ModelScreen::Cloud { provider, key, .. } => {
                    let Some(base_url) = provider_base(*provider) else {
                        return Activation::Ignore;
                    };
                    let Some(id) = PROVIDERS
                        .get(*provider)
                        .and_then(|p| p.models.get(index - 1))
                    else {
                        return Activation::Ignore;
                    };
                    Activation::Save {
                        base_url: base_url.to_string(),
                        model: (*id).to_string(),
                        token: Some(key.as_str().to_string()),
                    }
                }
                _ => Activation::Ignore,
            },
            Some(Row::Different) => match screen {
                ModelScreen::Cloud { provider, .. } if provider_base(*provider).is_none() => {
                    Activation::Ignore
                }
                _ => push_choice(),
            },
            Some(Row::Provider) => {
                let select = if provider_index(&section.base_url) == Some(index) {
                    listed_row(index, section.model.trim()).unwrap_or(0)
                } else {
                    0
                };
                Activation::Push {
                    screen: cloud_screen(
                        index,
                        section,
                        provider_index(&section.base_url) == Some(index),
                    ),
                    select,
                    fetch: false,
                }
            }
            Some(Row::ApiKey) | None => Activation::Ignore,
            Some(Row::OtherModel) => {
                let ModelScreen::Cloud {
                    provider,
                    key,
                    other,
                } = screen
                else {
                    return Activation::Ignore;
                };
                let Some(base_url) = provider_base(*provider) else {
                    return Activation::Ignore;
                };
                let model = other.as_str().trim().to_string();
                if model.is_empty() {
                    Activation::Ignore
                } else {
                    Activation::Save {
                        base_url: base_url.to_string(),
                        model,
                        token: Some(key.as_str().to_string()),
                    }
                }
            }
            Some(Row::BaseUrl | Row::ModelName | Row::Token) => match screen {
                ModelScreen::Fields {
                    base_url,
                    model,
                    token,
                    ..
                } => Activation::Save {
                    base_url: base_url.as_str().to_string(),
                    model: model.as_str().to_string(),
                    token: Some(token.as_str().to_string()),
                },
                _ => Activation::Ignore,
            },
        }
    }

    /// Apply a finished tag list to the open Local screen.
    ///
    /// A saved Ollama model name is merged into the fetched names. No names and
    /// nothing saved replaces Local with the typed form. A saved name with an
    /// empty or failed fetch stays on Local and shows that name.
    pub(crate) fn apply_tags(&mut self, names: Result<Vec<String>, String>) {
        self.pending_tags_id = None;
        if !self.on_local() {
            return;
        }
        let saved = match self.current() {
            Some(ModelScreen::Local { saved, .. }) => saved.clone(),
            _ => None,
        };
        let mut names = names.unwrap_or_default();
        if let Some(saved_name) = &saved
            && !names.iter().any(|name| name == saved_name)
        {
            names.insert(0, saved_name.clone());
        }
        if names.is_empty() {
            self.replace_local_with_fields();
            self.reset_highlight(0);
            return;
        }
        let highlight = saved
            .as_ref()
            .and_then(|saved_name| names.iter().position(|name| name == saved_name))
            .unwrap_or(0);
        if let Some(ModelScreen::Local { names: slot, .. }) = self.stack.last_mut() {
            *slot = names;
        }
        self.select_index(highlight);
        self.list.set_offset(highlight);
    }

    fn replace_local_with_fields(&mut self) {
        let Some(pos) = self
            .stack
            .iter()
            .rposition(|screen| matches!(screen, ModelScreen::Local { .. }))
        else {
            return;
        };
        let mut base_url = TextInput::default();
        base_url.set_text(OLLAMA_BASE_URL);
        self.stack[pos] = ModelScreen::Fields {
            base_url,
            model: TextInput::default(),
            token: TextInput::default(),
            note: Some(OLLAMA_EMPTY_NOTE),
        };
    }
}

impl ListNav for ModelSetupState {
    fn list(&self) -> &SelectableList {
        &self.list
    }

    fn list_mut(&mut self) -> &mut SelectableList {
        &mut self.list
    }

    fn item_count(&self) -> usize {
        self.current().map(ModelScreen::row_count).unwrap_or(0)
    }
}

/// Which control an index is on the current screen. Label, description, the
/// text field, and Enter all read this so the index boundaries live in one place.
#[derive(Clone, Copy)]
enum Row {
    Local,
    Cloud,
    OtherServer,
    /// A fetched Ollama name, or a listed cloud model id.
    Name,
    Different,
    Provider,
    ApiKey,
    OtherModel,
    BaseUrl,
    ModelName,
    Token,
}

impl ModelScreen {
    fn row_count(&self) -> usize {
        match self {
            Self::Choice => 3,
            Self::Local { names, .. } => names.len() + 1,
            Self::Providers => PROVIDERS.len(),
            Self::Cloud { provider, .. } => 1 + cloud_model_count(*provider) + 2,
            Self::Fields { .. } => 4,
        }
    }

    fn row(&self, index: usize) -> Option<Row> {
        match self {
            Self::Choice => match index {
                0 => Some(Row::Local),
                1 => Some(Row::Cloud),
                2 => Some(Row::OtherServer),
                _ => None,
            },
            Self::Local { names, .. } => {
                if index < names.len() {
                    Some(Row::Name)
                } else if index == names.len() {
                    Some(Row::Different)
                } else {
                    None
                }
            }
            Self::Providers => (index < PROVIDERS.len()).then_some(Row::Provider),
            Self::Cloud { provider, .. } => {
                let models = cloud_model_count(*provider);
                if index == 0 {
                    Some(Row::ApiKey)
                } else if index <= models {
                    Some(Row::Name)
                } else if index == models + 1 {
                    Some(Row::OtherModel)
                } else if index == models + 2 {
                    Some(Row::Different)
                } else {
                    None
                }
            }
            Self::Fields { .. } => match index {
                0 => Some(Row::BaseUrl),
                1 => Some(Row::ModelName),
                2 => Some(Row::Token),
                3 => Some(Row::Different),
                _ => None,
            },
        }
    }

    fn label(&self, index: usize) -> String {
        match self.row(index) {
            Some(Row::Local) => "Local".to_string(),
            Some(Row::Cloud) => "Cloud".to_string(),
            Some(Row::OtherServer) => "Other server".to_string(),
            Some(Row::Name) => self.name_at(index).unwrap_or_default(),
            Some(Row::Different) => DIFFERENT_SETUP.to_string(),
            Some(Row::Provider) => PROVIDERS
                .get(index)
                .map(|provider| provider.name)
                .unwrap_or("")
                .to_string(),
            Some(Row::ApiKey) => "API key".to_string(),
            Some(Row::OtherModel) => "Other model…".to_string(),
            Some(Row::BaseUrl) => "Base URL".to_string(),
            Some(Row::ModelName) => "Model".to_string(),
            Some(Row::Token) => "Token".to_string(),
            None => String::new(),
        }
    }

    fn name_at(&self, index: usize) -> Option<String> {
        match self {
            Self::Local { names, .. } => names.get(index).cloned(),
            Self::Cloud { provider, .. } => PROVIDERS
                .get(*provider)
                .and_then(|p| p.models.get(index - 1))
                .map(|id| (*id).to_string()),
            _ => None,
        }
    }

    fn description(&self, index: usize) -> String {
        match self.row(index) {
            Some(Row::Local) => "Models on this machine, from Ollama.".to_string(),
            Some(Row::Cloud) => "OpenAI, Groq, or OpenRouter.".to_string(),
            Some(Row::OtherServer) => "Type a base URL, a model name, and a token.".to_string(),
            Some(Row::Name) => "Use this model.".to_string(),
            Some(Row::Different) => "Choose a different way to set the model.".to_string(),
            Some(Row::Provider) => PROVIDERS
                .get(index)
                .map(|provider| format!("Models from {}.", provider.name))
                .unwrap_or_default(),
            Some(Row::ApiKey) => "Sent with requests. Enter does not save.".to_string(),
            Some(Row::OtherModel) => "A model id from this provider. Enter saves it.".to_string(),
            Some(Row::BaseUrl | Row::ModelName | Row::Token) => self.field_description(index),
            None => String::new(),
        }
    }

    fn field_description(&self, index: usize) -> String {
        if let Self::Fields {
            note: Some(note), ..
        } = self
        {
            return (*note).to_string();
        }
        match self.row(index) {
            Some(Row::BaseUrl) => "The OpenAI-compatible base URL.",
            Some(Row::ModelName) => "The model name on that server.",
            _ => "A bearer token. Blank is stored as none.",
        }
        .to_string()
    }

    fn field_id(&self, index: usize) -> Option<TextFieldId> {
        match self.row(index)? {
            Row::ApiKey => Some(TextFieldId::ModelCloudKey),
            Row::OtherModel | Row::ModelName => Some(TextFieldId::ModelName),
            Row::BaseUrl => Some(TextFieldId::ModelBaseUrl),
            Row::Token => Some(TextFieldId::ModelToken),
            _ => None,
        }
    }

    fn field_mut(&mut self, index: usize) -> Option<&mut TextInput> {
        let kind = self.row(index)?;
        match (self, kind) {
            (Self::Cloud { key, .. }, Row::ApiKey) => Some(key),
            (Self::Cloud { other, .. }, Row::OtherModel) => Some(other),
            (Self::Fields { base_url, .. }, Row::BaseUrl) => Some(base_url),
            (Self::Fields { model, .. }, Row::ModelName) => Some(model),
            (Self::Fields { token, .. }, Row::Token) => Some(token),
            _ => None,
        }
    }
}

fn push_choice() -> Activation {
    Activation::Push {
        screen: ModelScreen::Choice,
        select: 0,
        fetch: false,
    }
}

fn cloud_model_count(provider: usize) -> usize {
    PROVIDERS
        .get(provider)
        .map(|provider| provider.models.len())
        .unwrap_or(0)
}

fn provider_base(provider: usize) -> Option<&'static str> {
    PROVIDERS.get(provider).map(|provider| provider.base_url)
}

fn canonical_base(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

fn is_ollama(url: &str) -> bool {
    !canonical_base(url).is_empty() && canonical_base(url) == canonical_base(OLLAMA_BASE_URL)
}

fn provider_index(url: &str) -> Option<usize> {
    let url = canonical_base(url);
    if url.is_empty() {
        return None;
    }
    PROVIDERS
        .iter()
        .position(|provider| canonical_base(provider.base_url) == url)
}

fn ollama_saved(section: &ModelSection) -> Option<String> {
    if !is_ollama(&section.base_url) {
        return None;
    }
    let name = section.model.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn listed_row(provider: usize, model: &str) -> Option<usize> {
    PROVIDERS
        .get(provider)?
        .models
        .iter()
        .position(|id| *id == model)
        .map(|index| index + 1)
}

fn cloud_screen(provider: usize, section: &ModelSection, seed_key: bool) -> ModelScreen {
    let mut key = TextInput::default();
    if seed_key && let Some(token) = section.token.as_deref() {
        key.set_text(token.trim());
    }
    ModelScreen::Cloud {
        provider,
        key,
        other: TextInput::default(),
    }
}

fn fields_from(section: &ModelSection) -> ModelScreen {
    let mut token = TextInput::default();
    if let Some(value) = section.token.as_deref() {
        token.set_text(value.trim());
    }
    ModelScreen::Fields {
        base_url: TextInput::from(section.base_url.trim().to_string()),
        model: TextInput::from(section.model.trim().to_string()),
        token,
        note: None,
    }
}

fn empty_fields() -> ModelScreen {
    ModelScreen::Fields {
        base_url: TextInput::default(),
        model: TextInput::default(),
        token: TextInput::default(),
        note: None,
    }
}

fn landing(section: &ModelSection) -> (ModelScreen, usize) {
    let url = section.base_url.trim();
    let model = section.model.trim();
    if url.is_empty() && model.is_empty() {
        return (ModelScreen::Choice, 0);
    }
    if is_ollama(url) {
        return (
            ModelScreen::Local {
                names: Vec::new(),
                saved: ollama_saved(section),
            },
            0,
        );
    }
    if let Some(provider) = provider_index(url) {
        let select = listed_row(provider, model).unwrap_or(0);
        return (cloud_screen(provider, section, true), select);
    }
    (fields_from(section), 0)
}

fn blank_token(token: Option<String>) -> Option<String> {
    token.and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() { None } else { Some(value) }
    })
}

impl AppModel {
    pub(crate) fn model_setup_state(&self) -> Option<&ModelSetupState> {
        match &self.overlay {
            Overlay::ModelSetup(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) fn model_setup_state_mut(&mut self) -> Option<&mut ModelSetupState> {
        match &mut self.overlay {
            Overlay::ModelSetup(state) => Some(state),
            _ => None,
        }
    }

    /// Open model setup on the screen the saved section implies. The returned
    /// request is present only when that screen is Local.
    pub(crate) fn open_model_setup(&mut self) -> Option<OllamaTagsRequest> {
        let state = ModelSetupState::from_section(&self.services.config.model);
        let fetch = state.on_local();
        self.overlay = Overlay::ModelSetup(Box::new(state));
        fetch.then(|| self.issue_tags_request())
    }

    pub(crate) fn model_setup_select(&mut self, index: usize) {
        if let Some(state) = self.model_setup_state_mut() {
            state.select_index(index);
        }
    }

    /// Enter on the highlighted row. Pushing Local is the only arm that fetches.
    pub(crate) fn model_setup_activate(&mut self) -> Option<OllamaTagsRequest> {
        let Some(index) = self
            .model_setup_state()
            .and_then(|state| state.selected_index())
        else {
            return None;
        };
        let activation = self
            .model_setup_state()
            .map(|state| state.activation(index, &self.services.config.model))
            .unwrap_or(Activation::Ignore);
        match activation {
            Activation::Push {
                screen,
                select,
                fetch,
            } => {
                if let Some(state) = self.model_setup_state_mut() {
                    state.stack.push(screen);
                    state.reset_highlight(select);
                }
                fetch.then(|| self.issue_tags_request())
            }
            Activation::Save {
                base_url,
                model,
                token,
            } => {
                self.commit_model(base_url, model, token);
                None
            }
            Activation::Ignore => None,
        }
    }

    pub(crate) fn model_setup_cancel(&mut self) {
        let depth = self
            .model_setup_state()
            .map(|state| state.stack.len())
            .unwrap_or(0);
        if depth <= 1 {
            self.return_to_settings_model_row();
            return;
        }
        if let Some(state) = self.model_setup_state_mut() {
            state.stack.pop();
            state.reset_highlight(0);
        }
    }

    /// Accept a tag-list reply whose id is the one Local is waiting on.
    /// Anything else is ignored. Returns whether the dialog changed.
    pub(crate) fn accept_ollama_tags(
        &mut self,
        id: u64,
        names: Result<Vec<String>, String>,
    ) -> bool {
        let matches = self
            .model_setup_state()
            .is_some_and(|state| state.pending_tags_id == Some(id) && state.on_local());
        if !matches {
            return false;
        }
        if let Some(state) = self.model_setup_state_mut() {
            state.apply_tags(names);
        }
        true
    }

    fn issue_tags_request(&mut self) -> OllamaTagsRequest {
        let id = self.next_ollama_tags_id;
        self.next_ollama_tags_id = self.next_ollama_tags_id.wrapping_add(1);
        if let Some(state) = self.model_setup_state_mut() {
            state.pending_tags_id = Some(id);
        }
        OllamaTagsRequest { id }
    }

    fn commit_model(&mut self, base_url: String, model: String, token: Option<String>) {
        let base_url = base_url.trim().to_string();
        let model = model.trim().to_string();
        if base_url.is_empty() || model.is_empty() {
            return;
        }
        self.services.config.model.base_url = base_url;
        self.services.config.model.model = model;
        self.services.config.model.token = blank_token(token);
        if self.apply_setting_change(crate::tui::features::settings::AfterChange::None) {
            self.return_to_settings_model_row();
        }
    }
}
