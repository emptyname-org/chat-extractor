use crate::{
    AppError, ArchiveIndex, Conversation, ConversationExportMode, ExportFormat, MultiExportRequest,
    build_archive_index, export_conversations, export_conversations_overwriting,
};
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use gtk::glib::{self, ControlFlow};
use gtk::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, ButtonsType, Calendar,
    CheckButton, ColumnView, ColumnViewColumn, CustomSorter, Entry, EntryIconPosition,
    FileChooserAction, FileChooserNative, FileFilter, Image, Label, MenuButton, MessageDialog,
    MessageType, NoSelection, Orientation, PolicyType, Popover, ResponseType, ScrolledWindow,
    Separator, SignalListItemFactory, SortListModel, SortType, Spinner, Stack,
};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STEP_NAMES: [&str; 4] = ["introduction", "source", "conversations", "options"];
const STEP_TITLES: [&str; 4] = [
    "Export from Signal",
    "Select export directory",
    "Select chats",
    "Export options",
];
const CONFIG_DIRECTORY_NAME: &str = "chatextractor";
const PREFERENCES_FILE_NAME: &str = "preferences.json";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
struct AppPreferences {
    last_source_folder: Option<PathBuf>,
    last_destination_folder: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum RememberedFolder {
    Source,
    Destination,
}

#[derive(Clone)]
struct ConversationChoice {
    id: String,
    name: String,
    message_count: u64,
    selected: Rc<Cell<bool>>,
    first_timestamp_ms: Option<i64>,
    last_timestamp_ms: Option<i64>,
}

#[derive(Clone)]
struct DateInput {
    entry: Entry,
    calendar: Calendar,
}

#[derive(Default)]
struct AppState {
    index: Option<Arc<ArchiveIndex>>,
    loaded_source: Option<PathBuf>,
    choices: Vec<ConversationChoice>,
    busy: bool,
    suggested_filename: Option<String>,
    preferences: AppPreferences,
}

#[derive(Clone)]
struct StepIndicator {
    button: Button,
    status: Label,
    title: Label,
}

#[derive(Clone)]
struct Ui {
    window: ApplicationWindow,
    stack: Stack,
    steps: Vec<StepIndicator>,
    current_step: Rc<Cell<usize>>,
    max_unlocked_step: Rc<Cell<usize>>,
    back_button: Button,
    next_button: Button,
    spinner: Spinner,
    status_label: Label,
    source_entry: Entry,
    conversation_store: gtk::gio::ListStore,
    conversation_columns: [ColumnViewColumn; 3],
    selection_label: Label,
    conversation_filter: Entry,
    start_date: DateInput,
    end_date: DateInput,
    include_media: CheckButton,
    json_format: CheckButton,
    separate_mode: CheckButton,
    dialogs: Rc<RefCell<Vec<FileChooserNative>>>,
}

fn preferences_path() -> PathBuf {
    glib::user_config_dir()
        .join(CONFIG_DIRECTORY_NAME)
        .join(PREFERENCES_FILE_NAME)
}

fn load_preferences_from(path: &Path) -> AppPreferences {
    File::open(path)
        .ok()
        .and_then(|file| serde_json::from_reader(BufReader::new(file)).ok())
        .unwrap_or_default()
}

fn save_preferences_to(path: &Path, preferences: &AppPreferences) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "preferences path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".{PREFERENCES_FILE_NAME}.{}-{nonce}.tmp",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let result = serde_json::to_writer(&mut file, preferences)
        .map_err(io::Error::other)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all());
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

fn remember_folder(state: &Rc<RefCell<AppState>>, kind: RememberedFolder, path: &Path) {
    let preferences = {
        let mut state = state.borrow_mut();
        match kind {
            RememberedFolder::Source => {
                state.preferences.last_source_folder = Some(path.to_path_buf())
            }
            RememberedFolder::Destination => {
                state.preferences.last_destination_folder = Some(path.to_path_buf())
            }
        }
        state.preferences.clone()
    };
    let _ = save_preferences_to(&preferences_path(), &preferences);
}

pub fn build_ui(application: &Application) {
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path("/org/signal_filter/icons");
    }
    install_wizard_styles();
    let preferences = load_preferences_from(&preferences_path());

    let window = ApplicationWindow::builder()
        .application(application)
        .title("Chat Extractor for Signal")
        .icon_name("signal-filter")
        .default_width(1040)
        .default_height(720)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 0);
    let body = GtkBox::new(Orientation::Horizontal, 0);
    body.set_vexpand(true);

    let rail = GtkBox::new(Orientation::Vertical, 10);
    rail.add_css_class("wizard-navigation");
    rail.set_width_request(255);
    rail.set_margin_top(26);
    rail.set_margin_bottom(24);
    rail.set_margin_start(22);
    rail.set_margin_end(20);

    let brand = GtkBox::new(Orientation::Horizontal, 12);
    brand.set_margin_bottom(24);
    let brand_icon = Image::from_icon_name("signal-filter");
    brand_icon.set_pixel_size(56);
    let brand_name = Label::new(Some("Chat Extractor for Signal"));
    brand_name.set_xalign(0.0);
    brand_name.set_wrap(true);
    brand_name.add_css_class("title-3");
    brand.append(&brand_icon);
    brand.append(&brand_name);
    rail.append(&brand);

    let mut step_indicators = Vec::new();
    for title in STEP_TITLES {
        let status = Label::new(Some("○"));
        status.set_width_chars(2);
        status.set_xalign(0.5);
        let title_label = Label::new(Some(title));
        title_label.set_xalign(0.0);
        title_label.set_hexpand(true);

        let row = GtkBox::new(Orientation::Horizontal, 10);
        row.append(&status);
        row.append(&title_label);
        let button = Button::builder().child(&row).halign(Align::Fill).build();
        button.add_css_class("flat");
        button.set_focusable(false);
        button.set_has_frame(false);
        rail.append(&button);
        step_indicators.push(StepIndicator {
            button,
            status,
            title: title_label,
        });
    }

    let stack = Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .transition_duration(180)
        .build();
    stack.add_css_class("wizard-content");
    stack.add_named(&build_introduction_page(), Some(STEP_NAMES[0]));

    let (source_page, source_entry, choose_source_button) = build_source_page();
    stack.add_named(&source_page, Some(STEP_NAMES[1]));

    let selection_label = Label::new(Some("0 chats selected"));
    selection_label.set_xalign(1.0);
    selection_label.add_css_class("dim-label");
    let (
        conversation_page,
        conversation_filter,
        select_all_button,
        select_none_button,
        conversation_store,
        conversation_columns,
    ) = build_conversation_page();
    stack.add_named(&conversation_page, Some(STEP_NAMES[2]));

    let start_date = build_date_input(Local::now().date_naive());
    let end_date = build_date_input(Local::now().date_naive());
    let include_media = CheckButton::with_label("Include media files");
    let markdown_format = CheckButton::with_label("Markdown (.md)");
    let json_format = CheckButton::with_label("JSON (.json)");
    json_format.set_group(Some(&markdown_format));
    markdown_format.set_active(true);
    let combined_mode = CheckButton::with_label("One combined file");
    let separate_mode = CheckButton::with_label("Separate file for each chat");
    separate_mode.set_group(Some(&combined_mode));
    separate_mode.set_active(true);
    let entire_range_button = Button::with_label("Entire range");
    stack.add_named(
        &build_options_page(
            &start_date,
            &end_date,
            &entire_range_button,
            &include_media,
            &markdown_format,
            &json_format,
            &combined_mode,
            &separate_mode,
        ),
        Some(STEP_NAMES[3]),
    );

    body.append(&rail);
    body.append(&Separator::new(Orientation::Vertical));
    body.append(&stack);
    root.append(&body);
    root.append(&Separator::new(Orientation::Horizontal));

    let bottom_bar = GtkBox::new(Orientation::Horizontal, 12);
    bottom_bar.add_css_class("wizard-footer");
    bottom_bar.set_margin_top(14);
    bottom_bar.set_margin_bottom(14);
    bottom_bar.set_margin_start(20);
    bottom_bar.set_margin_end(20);
    let spinner = Spinner::new();
    let status_label = Label::new(None);
    status_label.set_xalign(0.0);
    status_label.set_hexpand(true);
    let back_button = Button::with_label("Back");
    let next_button = Button::with_label("Next");
    for button in [&back_button, &next_button] {
        button.set_size_request(122, 44);
        button.add_css_class("pill");
    }
    next_button.add_css_class("suggested-action");
    bottom_bar.append(&spinner);
    bottom_bar.append(&status_label);
    bottom_bar.append(&selection_label);
    bottom_bar.append(&back_button);
    bottom_bar.append(&next_button);
    root.append(&bottom_bar);
    window.set_child(Some(&root));

    let ui = Ui {
        window,
        stack,
        steps: step_indicators,
        current_step: Rc::new(Cell::new(0)),
        max_unlocked_step: Rc::new(Cell::new(0)),
        back_button,
        next_button,
        spinner,
        status_label,
        source_entry,
        conversation_store,
        conversation_columns,
        selection_label,
        conversation_filter,
        start_date,
        end_date,
        include_media,
        json_format,
        separate_mode,
        dialogs: Rc::new(RefCell::new(Vec::new())),
    };
    let state = Rc::new(RefCell::new(AppState {
        preferences,
        ..AppState::default()
    }));
    install_conversation_factories(&ui, &state);

    connect_step_buttons(&ui, &state);

    {
        let ui = ui.clone();
        let state = state.clone();
        choose_source_button.connect_clicked(move |_| show_source_dialog(&ui, &state));
    }
    {
        let ui = ui.clone();
        let state = state.clone();
        select_all_button.connect_clicked(move |_| {
            let filter = ui.conversation_filter.text().to_lowercase();
            let selections = state
                .borrow()
                .choices
                .iter()
                .filter(|choice| conversation_matches_filter(choice, &filter))
                .map(|choice| choice.selected.clone())
                .collect::<Vec<_>>();
            for selected in selections {
                selected.set(true);
            }
            refresh_conversation_list(&ui, &state);
            refresh_selection(&ui, &state, true);
        });
    }
    {
        let ui = ui.clone();
        let state = state.clone();
        select_none_button.connect_clicked(move |_| {
            let filter = ui.conversation_filter.text().to_lowercase();
            let selections = state
                .borrow()
                .choices
                .iter()
                .filter(|choice| conversation_matches_filter(choice, &filter))
                .map(|choice| choice.selected.clone())
                .collect::<Vec<_>>();
            for selected in selections {
                selected.set(false);
            }
            refresh_conversation_list(&ui, &state);
            refresh_selection(&ui, &state, true);
        });
    }
    {
        let ui = ui.clone();
        let state = state.clone();
        ui.conversation_filter
            .clone()
            .connect_changed(move |_| refresh_conversation_list(&ui, &state));
    }
    {
        let ui = ui.clone();
        let state = state.clone();
        entire_range_button.connect_clicked(move |_| refresh_selection(&ui, &state, true));
    }
    {
        let ui = ui.clone();
        let state = state.clone();
        ui.json_format
            .clone()
            .connect_toggled(move |_| update_suggested_filename(&ui, &state));
    }
    {
        let ui = ui.clone();
        let state = state.clone();
        ui.separate_mode
            .clone()
            .connect_toggled(move |_| update_suggested_filename(&ui, &state));
    }
    {
        let ui = ui.clone();
        let state = state.clone();
        ui.back_button.clone().connect_clicked(move |_| {
            let current = ui.current_step.get();
            if current > 0 {
                show_step(&ui, &state, current - 1);
            }
        });
    }
    {
        let ui = ui.clone();
        let state = state.clone();
        ui.next_button
            .clone()
            .connect_clicked(move |_| advance_or_export(&ui, &state));
    }

    refresh_navigation(&ui, &state);
    ui.window.present();
}

fn install_wizard_styles() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        ".wizard-navigation, .wizard-footer { background-color: @theme_bg_color; }\
         .wizard-content { background-color: @theme_base_color; }\
         scrollbar slider { min-width: 12px; min-height: 12px; }\
         columnview.view > header > button sort-indicator.ascending {\
           -gtk-icon-source: -gtk-icontheme('pan-up-symbolic');\
           min-width: 16px; min-height: 16px;\
         }\
         columnview.view > header > button sort-indicator.descending {\
           -gtk-icon-source: -gtk-icontheme('pan-down-symbolic');\
           min-width: 16px; min-height: 16px;\
         }\
         calendar.view > grid > label.day-number:selected {\
           background-color: @theme_selected_bg_color;\
           color: @theme_selected_fg_color;\
           border-radius: 9999px;\
         }",
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn build_introduction_page() -> GtkBox {
    let page = page_shell(
        "Export chats from Signal",
        "First export chat history from Signal Desktop.",
    );

    let instructions = GtkBox::new(Orientation::Vertical, 14);
    instructions.set_margin_top(10);
    for (number, text) in [
        ("1", "Open Signal Desktop."),
        ("2", "Open Settings, then choose Chats."),
        ("3", "Select Export chat history, then choose a folder."),
        (
            "4",
            "Wait for Signal to finish, then select that folder here.",
        ),
    ] {
        let row = GtkBox::new(Orientation::Horizontal, 14);
        let number_label = Label::new(Some(number));
        number_label.set_width_chars(2);
        number_label.add_css_class("title-4");
        let text_label = Label::new(Some(text));
        text_label.set_xalign(0.0);
        text_label.set_wrap(true);
        text_label.set_hexpand(true);
        row.append(&number_label);
        row.append(&text_label);
        instructions.append(&row);
    }
    let note = Label::new(Some(
        "The folder contains main.jsonl and optional media. This app reads them locally and uploads nothing.",
    ));
    note.set_xalign(0.0);
    note.set_wrap(true);
    note.set_margin_top(12);
    note.add_css_class("dim-label");
    instructions.append(&note);
    page.append(&instructions);
    page
}

fn build_source_page() -> (GtkBox, Entry, Button) {
    let page = page_shell(
        "Select the Signal export",
        "Choose the folder; main.jsonl is found automatically.",
    );
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let entry = Entry::builder()
        .hexpand(true)
        .placeholder_text("Signal export folder")
        .build();
    entry.set_editable(false);
    let choose = Button::with_label("Choose folder…");
    row.append(&entry);
    row.append(&choose);
    page.append(&row);
    (page, entry, choose)
}

fn build_conversation_page() -> (
    GtkBox,
    Entry,
    Button,
    Button,
    gtk::gio::ListStore,
    [ColumnViewColumn; 3],
) {
    let page = page_shell(
        "Select chats",
        "Empty contacts and chats with only one Signal update are hidden.",
    );

    let filter = Entry::builder()
        .hexpand(true)
        .placeholder_text("Filter chat names")
        .build();
    filter.set_icon_from_icon_name(EntryIconPosition::Primary, Some("system-search-symbolic"));
    let controls = GtkBox::new(Orientation::Horizontal, 8);
    let select_all = Button::with_label("Select all");
    let select_none = Button::with_label("Select none");
    controls.append(&filter);
    controls.append(&select_all);
    controls.append(&select_none);
    page.append(&controls);

    let store = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
    let sort_model = SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
    let selection = NoSelection::new(Some(sort_model.clone()));
    let view = ColumnView::new(Some(selection));
    view.set_hexpand(true);
    view.set_vexpand(true);
    view.set_reorderable(false);
    view.set_single_click_activate(true);
    view.set_show_column_separators(true);
    view.set_show_row_separators(false);

    let name_column = ColumnViewColumn::new(Some("Name"), None::<SignalListItemFactory>);
    name_column.set_expand(true);
    name_column.set_resizable(true);
    let date_column = ColumnViewColumn::new(Some("Date"), None::<SignalListItemFactory>);
    date_column.set_fixed_width(145);
    date_column.set_resizable(true);
    let messages_column = ColumnViewColumn::new(Some("Messages"), None::<SignalListItemFactory>);
    messages_column.set_fixed_width(115);
    messages_column.set_resizable(true);

    name_column.set_sorter(Some(&CustomSorter::new(|left, right| {
        compare_conversation_objects(left, right, |left, right| {
            left.name.to_lowercase().cmp(&right.name.to_lowercase())
        })
    })));
    date_column.set_sorter(Some(&CustomSorter::new(|left, right| {
        compare_conversation_objects(left, right, |left, right| {
            left.last_timestamp_ms
                .cmp(&right.last_timestamp_ms)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        })
    })));
    messages_column.set_sorter(Some(&CustomSorter::new(|left, right| {
        compare_conversation_objects(left, right, |left, right| {
            left.message_count
                .cmp(&right.message_count)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        })
    })));

    view.append_column(&name_column);
    view.append_column(&messages_column);
    view.append_column(&date_column);
    if let Some(sorter) = view.sorter() {
        sort_model.set_sorter(Some(&sorter));
    }
    view.sort_by_column(Some(&date_column), SortType::Descending);

    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .has_frame(true)
        .hexpand(true)
        .vexpand(true)
        .min_content_height(360)
        .child(&view)
        .build();
    page.append(&scroller);
    (
        page,
        filter,
        select_all,
        select_none,
        store,
        [name_column, messages_column, date_column],
    )
}

fn compare_conversation_objects(
    left: &glib::Object,
    right: &glib::Object,
    compare: impl FnOnce(&ConversationChoice, &ConversationChoice) -> std::cmp::Ordering,
) -> gtk::Ordering {
    let left = left
        .downcast_ref::<glib::BoxedAnyObject>()
        .expect("conversation model item must be boxed")
        .borrow::<ConversationChoice>();
    let right = right
        .downcast_ref::<glib::BoxedAnyObject>()
        .expect("conversation model item must be boxed")
        .borrow::<ConversationChoice>();
    match compare(&left, &right) {
        std::cmp::Ordering::Less => gtk::Ordering::Smaller,
        std::cmp::Ordering::Equal => gtk::Ordering::Equal,
        std::cmp::Ordering::Greater => gtk::Ordering::Larger,
    }
}

fn install_conversation_factories(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    let name_factory = SignalListItemFactory::new();
    name_factory.connect_setup(|_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = GtkBox::new(Orientation::Horizontal, 8);
        list_item.set_child(Some(&cell));
    });
    name_factory.connect_bind(|_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(item) = list_item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let choice = item.borrow::<ConversationChoice>().clone();
        let Some(cell) = list_item.child().and_downcast::<GtkBox>() else {
            return;
        };
        clear_box(&cell);

        let check = CheckButton::new();
        check.set_active(choice.selected.get());
        check.set_can_target(false);
        check.set_focusable(false);
        check.set_tooltip_text(Some("Include this chat"));
        let name = Label::new(Some(&choice.name));
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_hexpand(true);
        cell.append(&check);
        cell.append(&name);
    });
    ui.conversation_columns[0].set_factory(Some(&name_factory));

    let date_factory = SignalListItemFactory::new();
    date_factory.connect_setup(|_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = Label::new(None);
        label.set_xalign(0.0);
        list_item.set_child(Some(&label));
    });
    date_factory.connect_bind(|_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(item) = list_item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let choice = item.borrow::<ConversationChoice>();
        let Some(label) = list_item.child().and_downcast::<Label>() else {
            return;
        };
        let text = choice
            .last_timestamp_ms
            .and_then(timestamp_date)
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| String::from("—"));
        label.set_text(&text);
    });
    ui.conversation_columns[2].set_factory(Some(&date_factory));

    let messages_factory = SignalListItemFactory::new();
    messages_factory.connect_setup(|_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = Label::new(None);
        label.set_xalign(0.0);
        list_item.set_child(Some(&label));
    });
    messages_factory.connect_bind(|_, list_item| {
        let Some(list_item) = list_item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(item) = list_item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let choice = item.borrow::<ConversationChoice>();
        let Some(label) = list_item.child().and_downcast::<Label>() else {
            return;
        };
        label.set_text(&choice.message_count.to_string());
    });
    ui.conversation_columns[1].set_factory(Some(&messages_factory));

    if let Some(view) = ui.conversation_columns[0].column_view() {
        let ui = ui.clone();
        let state = state.clone();
        view.connect_activate(move |view, position| {
            if toggle_visible_conversation(view, position) {
                refresh_conversation_list(&ui, &state);
                refresh_selection(&ui, &state, true);
            }
        });
    }
}

fn toggle_visible_conversation(view: &ColumnView, position: u32) -> bool {
    let Some(choice) = view
        .model()
        .and_then(|model| model.item(position))
        .and_downcast::<glib::BoxedAnyObject>()
    else {
        return false;
    };
    let selected = choice.borrow::<ConversationChoice>().selected.clone();
    selected.set(!selected.get());
    true
}

#[allow(clippy::too_many_arguments)]
fn build_options_page(
    start_date: &DateInput,
    end_date: &DateInput,
    entire_range_button: &Button,
    include_media: &CheckButton,
    markdown_format: &CheckButton,
    json_format: &CheckButton,
    combined_mode: &CheckButton,
    separate_mode: &CheckButton,
) -> GtkBox {
    let page = page_shell("Export options", "Type YYYY-MM-DD or use the calendars.");

    let dates = GtkBox::new(Orientation::Horizontal, 20);
    dates.append(&date_input_group("Start date", start_date));
    dates.append(&date_input_group("End date", end_date));
    entire_range_button.set_valign(Align::End);
    dates.append(entire_range_button);
    page.append(&dates);

    page.append(&option_group(
        "Files",
        &[separate_mode, combined_mode, include_media],
    ));
    page.append(&option_group("Format", &[markdown_format, json_format]));
    page
}

fn build_date_input(date: NaiveDate) -> DateInput {
    let entry = Entry::builder()
        .width_chars(12)
        .max_width_chars(12)
        .placeholder_text("YYYY-MM-DD")
        .build();
    entry.set_text(&date.format("%Y-%m-%d").to_string());
    let calendar = Calendar::new();
    select_calendar_day(&calendar, date);
    mark_calendar_day(&calendar, date);
    DateInput { entry, calendar }
}

fn date_input_group(title: &str, input: &DateInput) -> GtkBox {
    let group = GtkBox::new(Orientation::Vertical, 7);
    let label = Label::new(Some(title));
    label.set_xalign(0.0);
    label.add_css_class("heading");
    let row = GtkBox::new(Orientation::Horizontal, 6);
    let menu = MenuButton::new();
    menu.set_icon_name("x-office-calendar-symbolic");
    menu.set_tooltip_text(Some("Open calendar"));

    let popover = Popover::new();
    let popover_box = GtkBox::new(Orientation::Vertical, 8);
    popover_box.set_margin_top(10);
    popover_box.set_margin_bottom(10);
    popover_box.set_margin_start(10);
    popover_box.set_margin_end(10);
    popover_box.append(&input.calendar);
    let use_button = Button::with_label("Use date");
    use_button.add_css_class("suggested-action");
    popover_box.append(&use_button);
    popover.set_child(Some(&popover_box));
    menu.set_popover(Some(&popover));

    {
        let entry = input.entry.clone();
        let calendar = input.calendar.clone();
        let popover = popover.clone();
        use_button.connect_clicked(move |_| {
            if let Some(date) = calendar_date(&calendar) {
                entry.set_text(&date.format("%Y-%m-%d").to_string());
            }
            popover.popdown();
        });
    }
    {
        let entry = input.entry.clone();
        input.calendar.connect_day_selected(move |calendar| {
            let Some(date) = calendar_date(calendar) else {
                return;
            };
            mark_calendar_day(calendar, date);
            let date = date.format("%Y-%m-%d").to_string();
            if entry.text().as_str() != date {
                entry.set_text(&date);
            }
        });
    }
    {
        let calendar = input.calendar.clone();
        input.entry.connect_changed(move |entry| {
            if let Ok(date) = NaiveDate::parse_from_str(entry.text().as_str(), "%Y-%m-%d")
                && calendar_date(&calendar) != Some(date)
            {
                select_calendar_day(&calendar, date);
            }
        });
    }

    row.append(&input.entry);
    row.append(&menu);
    group.append(&label);
    group.append(&row);
    group
}

fn page_shell(title: &str, subtitle: &str) -> GtkBox {
    let page = GtkBox::new(Orientation::Vertical, 18);
    page.set_margin_top(40);
    page.set_margin_bottom(32);
    page.set_margin_start(42);
    page.set_margin_end(42);
    let title_label = Label::new(Some(title));
    title_label.set_xalign(0.0);
    title_label.add_css_class("title-2");
    let subtitle_label = Label::new(Some(subtitle));
    subtitle_label.set_xalign(0.0);
    subtitle_label.set_wrap(true);
    subtitle_label.add_css_class("dim-label");
    page.append(&title_label);
    page.append(&subtitle_label);
    page.append(&Separator::new(Orientation::Horizontal));
    page
}

fn option_group(title: &str, controls: &[&CheckButton]) -> GtkBox {
    let group = GtkBox::new(Orientation::Vertical, 7);
    let label = Label::new(Some(title));
    label.set_xalign(0.0);
    label.add_css_class("heading");
    group.append(&label);
    for control in controls {
        group.append(*control);
    }
    group
}

fn connect_step_buttons(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    for (position, indicator) in ui.steps.iter().enumerate() {
        let ui = ui.clone();
        let state = state.clone();
        indicator.button.connect_clicked(move |_| {
            if position <= ui.max_unlocked_step.get() {
                show_step(&ui, &state, position);
            }
        });
    }
}

fn show_step(ui: &Ui, state: &Rc<RefCell<AppState>>, step: usize) {
    ui.current_step.set(step.min(STEP_NAMES.len() - 1));
    ui.stack
        .set_visible_child_name(STEP_NAMES[ui.current_step.get()]);
    refresh_navigation(ui, state);
}

fn refresh_navigation(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    let state = state.borrow();
    let current = ui.current_step.get();
    let has_selection = state.choices.iter().any(|choice| choice.selected.get());
    let entered_source = PathBuf::from(ui.source_entry.text().as_str());
    let can_continue = match current {
        0 => true,
        1 => state.index.is_some() && state.loaded_source.as_ref() == Some(&entered_source),
        2 => has_selection,
        3 => has_selection,
        _ => false,
    } && !state.busy;

    ui.back_button.set_sensitive(current > 0 && !state.busy);
    ui.next_button.set_sensitive(can_continue);
    ui.selection_label
        .set_visible(selection_status_is_visible(current));
    ui.next_button
        .set_label(if current == 3 { "Export" } else { "Next" });

    for (position, indicator) in ui.steps.iter().enumerate() {
        indicator.status.set_text(if position < current {
            "✓"
        } else if position == current {
            "●"
        } else {
            "○"
        });
        if position == current {
            indicator
                .title
                .set_markup(&format!("<b>{}</b>", STEP_TITLES[position]));
        } else {
            indicator.title.set_text(STEP_TITLES[position]);
        }
        indicator
            .button
            .set_sensitive(position <= ui.max_unlocked_step.get() && !state.busy);
        indicator
            .button
            .set_opacity(if position <= ui.max_unlocked_step.get() {
                1.0
            } else {
                0.45
            });
    }
}

fn selection_status_is_visible(step: usize) -> bool {
    step >= 2
}

fn advance_or_export(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    match ui.current_step.get() {
        0 => unlock_and_show(ui, state, 1),
        1 if state.borrow().index.is_some() => unlock_and_show(ui, state, 2),
        2 if state
            .borrow()
            .choices
            .iter()
            .any(|choice| choice.selected.get()) =>
        {
            refresh_selection(ui, state, true);
            unlock_and_show(ui, state, 3);
        }
        3 if validate_date_range(ui).is_some() => {
            update_suggested_filename(ui, state);
            choose_export_destination(ui, state);
        }
        _ => {}
    }
}

fn unlock_and_show(ui: &Ui, state: &Rc<RefCell<AppState>>, step: usize) {
    ui.max_unlocked_step
        .set(ui.max_unlocked_step.get().max(step));
    show_step(ui, state, step);
}

fn set_busy(ui: &Ui, state: &Rc<RefCell<AppState>>, busy: bool, message: &str) {
    state.borrow_mut().busy = busy;
    ui.stack.set_sensitive(!busy);
    ui.status_label.set_text(message);
    if busy {
        ui.spinner.start();
    } else {
        ui.spinner.stop();
    }
    refresh_navigation(ui, state);
}

fn show_source_dialog(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    if state.borrow().busy {
        return;
    }
    let entered = PathBuf::from(ui.source_entry.text().as_str());
    let initial = if entered.is_dir() {
        entered
    } else {
        state
            .borrow()
            .preferences
            .last_source_folder
            .clone()
            .unwrap_or_default()
    };
    let initial = usable_initial_directory(&initial);
    let ui_for_selection = ui.clone();
    let state_for_selection = state.clone();
    present_folder_dialog(ui, "Select Signal export folder", &initial, move |path| {
        remember_folder(&state_for_selection, RememberedFolder::Source, path);
        ui_for_selection
            .source_entry
            .set_text(&path.to_string_lossy());
        start_archive_load(&ui_for_selection, &state_for_selection);
    });
}

fn choose_export_destination(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    if state.borrow().busy {
        return;
    }
    let selected_count = state
        .borrow()
        .choices
        .iter()
        .filter(|choice| choice.selected.get())
        .count();
    if selected_count == 0 {
        return;
    }
    let multiple_files =
        export_uses_directory_chooser(ui.separate_mode.is_active(), selected_count);
    let suggested_filename = state
        .borrow()
        .suggested_filename
        .clone()
        .unwrap_or_else(|| format!("chat-export.{}", filename_extension(selected_format(ui))));
    let initial = state
        .borrow()
        .preferences
        .last_destination_folder
        .clone()
        .unwrap_or_default();
    let initial = usable_initial_directory(&initial);

    if multiple_files {
        let ui_for_selection = ui.clone();
        let state_for_selection = state.clone();
        let suggested_filename = suggested_filename.clone();
        present_export_folder_dialog(ui, &initial, move |directory| {
            remember_folder(
                &state_for_selection,
                RememberedFolder::Destination,
                directory,
            );
            start_export_to(
                &ui_for_selection,
                &state_for_selection,
                directory.join(&suggested_filename),
            );
        });
    } else {
        let ui_for_selection = ui.clone();
        let state_for_selection = state.clone();
        present_save_dialog(
            ui,
            &initial,
            &suggested_filename,
            selected_format(ui),
            move |output_file| {
                let output_file =
                    normalized_output_path(output_file, selected_format(&ui_for_selection));
                if let Some(directory) = output_file.parent() {
                    remember_folder(
                        &state_for_selection,
                        RememberedFolder::Destination,
                        directory,
                    );
                }
                start_export_to(&ui_for_selection, &state_for_selection, output_file);
            },
        );
    }
}

fn export_uses_directory_chooser(separate: bool, selected_count: usize) -> bool {
    separate && selected_count > 1
}

fn usable_initial_directory(requested: &Path) -> PathBuf {
    if requested.is_dir() {
        return requested.to_path_buf();
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn present_folder_dialog<F>(ui: &Ui, title: &str, initial: &Path, on_selected: F)
where
    F: Fn(&Path) + 'static,
{
    let dialog = build_folder_dialog(&ui.window, title, initial);
    let dialogs = ui.dialogs.clone();
    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Accept
            && let Some(path) = dialog.file().and_then(|file| file.path())
        {
            on_selected(&path);
        }
        dialog.destroy();
        dialogs.borrow_mut().retain(|open| open != dialog);
    });
    ui.dialogs.borrow_mut().push(dialog.clone());
    dialog.show();
}

fn present_export_folder_dialog<F>(ui: &Ui, initial: &Path, on_selected: F)
where
    F: Fn(&Path) + 'static,
{
    let dialog =
        build_folder_dialog_with_accept(&ui.window, "Export chats to a folder", initial, "Export");
    let dialogs = ui.dialogs.clone();
    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Accept
            && let Some(path) = dialog.file().and_then(|file| file.path())
        {
            on_selected(&path);
        }
        dialog.destroy();
        dialogs.borrow_mut().retain(|open| open != dialog);
    });
    ui.dialogs.borrow_mut().push(dialog.clone());
    dialog.show();
}

fn present_save_dialog<F>(
    ui: &Ui,
    initial: &Path,
    suggested_filename: &str,
    format: ExportFormat,
    on_selected: F,
) where
    F: Fn(&Path) + 'static,
{
    let dialog = build_save_dialog(&ui.window, initial, suggested_filename, format);
    let dialogs = ui.dialogs.clone();
    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Accept
            && let Some(path) = dialog.file().and_then(|file| file.path())
        {
            on_selected(&path);
        }
        dialog.destroy();
        dialogs.borrow_mut().retain(|open| open != dialog);
    });
    ui.dialogs.borrow_mut().push(dialog.clone());
    dialog.show();
}

fn build_folder_dialog(
    parent: &ApplicationWindow,
    title: &str,
    initial: &Path,
) -> FileChooserNative {
    build_folder_dialog_with_accept(parent, title, initial, "Select")
}

fn build_folder_dialog_with_accept(
    parent: &ApplicationWindow,
    title: &str,
    initial: &Path,
    accept_label: &str,
) -> FileChooserNative {
    let dialog = FileChooserNative::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .action(FileChooserAction::SelectFolder)
        .accept_label(accept_label)
        .cancel_label("Cancel")
        .build();
    let initial_file = gtk::gio::File::for_path(initial);
    let _ = dialog.set_current_folder(Some(&initial_file));
    dialog
}

fn build_save_dialog(
    parent: &ApplicationWindow,
    initial: &Path,
    suggested_filename: &str,
    format: ExportFormat,
) -> FileChooserNative {
    let dialog = FileChooserNative::builder()
        .title("Export chats")
        .transient_for(parent)
        .modal(true)
        .action(FileChooserAction::Save)
        .accept_label("Export")
        .cancel_label("Cancel")
        .build();
    let initial_file = gtk::gio::File::for_path(initial);
    let _ = dialog.set_current_folder(Some(&initial_file));
    dialog.set_current_name(suggested_filename);
    let filter = FileFilter::new();
    filter.set_name(Some(match format {
        ExportFormat::Markdown => "Markdown files",
        ExportFormat::Json => "JSON files",
    }));
    filter.add_pattern(&format!("*.{}", filename_extension(format)));
    dialog.add_filter(&filter);
    dialog
}

fn start_archive_load(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    if state.borrow().busy {
        return;
    }
    let folder = PathBuf::from(ui.source_entry.text().as_str());
    if folder.as_os_str().is_empty() {
        show_message(
            &ui.window,
            MessageType::Warning,
            "Select an export folder first.",
            None,
        );
        return;
    }

    {
        let mut state = state.borrow_mut();
        state.index = None;
        state.loaded_source = None;
        state.choices.clear();
    }
    ui.conversation_store.remove_all();
    set_busy(ui, state, true, "Reading Signal export…");

    let loaded_folder = folder.clone();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(build_archive_index(&folder));
    });

    let ui = ui.clone();
    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        match receiver.try_recv() {
            Ok(Ok(index)) => {
                populate_conversations(&ui, &state, index, loaded_folder.clone());
                set_busy(&ui, &state, false, "Export loaded.");
                unlock_and_show(&ui, &state, 2);
                ControlFlow::Break
            }
            Ok(Err(error)) => {
                set_busy(&ui, &state, false, "Could not load the export.");
                show_app_error(&ui.window, "Could not load the Signal export", &error);
                ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                set_busy(&ui, &state, false, "Loading stopped unexpectedly.");
                show_message(
                    &ui.window,
                    MessageType::Error,
                    "Loading stopped unexpectedly.",
                    None,
                );
                ControlFlow::Break
            }
        }
    });
}

fn populate_conversations(
    ui: &Ui,
    state: &Rc<RefCell<AppState>>,
    index: ArchiveIndex,
    loaded_source: PathBuf,
) {
    ui.conversation_store.remove_all();
    let index = Arc::new(index);
    let mut choices = Vec::new();
    for conversation in index
        .conversations
        .values()
        .filter(|conversation| conversation_is_selectable(conversation))
    {
        choices.push(ConversationChoice {
            id: conversation.id.clone(),
            name: conversation.name.clone(),
            message_count: conversation.message_count,
            selected: Rc::new(Cell::new(false)),
            first_timestamp_ms: conversation.first_timestamp_ms,
            last_timestamp_ms: conversation.last_timestamp_ms,
        });
    }

    let mut app_state = state.borrow_mut();
    app_state.index = Some(index);
    app_state.loaded_source = Some(loaded_source);
    app_state.choices = choices;
    drop(app_state);
    ui.conversation_filter.set_text("");
    refresh_conversation_list(ui, state);
}

fn conversation_is_selectable(conversation: &Conversation) -> bool {
    conversation.message_count > 0 && !conversation.is_technical_update_only
}

fn conversation_matches_filter(choice: &ConversationChoice, filter: &str) -> bool {
    filter.is_empty() || choice.name.to_lowercase().contains(filter)
}

fn refresh_conversation_list(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    ui.conversation_store.remove_all();
    let filter = ui.conversation_filter.text().to_lowercase();
    let state_ref = state.borrow();
    let choices = state_ref
        .choices
        .iter()
        .filter(|choice| conversation_matches_filter(choice, &filter))
        .collect::<Vec<_>>();

    for choice in &choices {
        ui.conversation_store
            .append(&glib::BoxedAnyObject::new((*choice).clone()));
    }

    drop(state_ref);
    refresh_selection_label(ui, state);
    update_suggested_filename(ui, state);
    refresh_navigation(ui, state);
}

fn refresh_selection(ui: &Ui, state: &Rc<RefCell<AppState>>, set_dates: bool) {
    let state_ref = state.borrow();
    let selected = state_ref
        .choices
        .iter()
        .filter(|choice| choice.selected.get())
        .collect::<Vec<_>>();
    if set_dates && !selected.is_empty() {
        let first = selected
            .iter()
            .filter_map(|choice| choice.first_timestamp_ms)
            .filter(|timestamp| *timestamp > 0)
            .min();
        let last = selected
            .iter()
            .filter_map(|choice| choice.last_timestamp_ms)
            .filter(|timestamp| *timestamp > 0)
            .max();
        if let Some(date) = first.and_then(timestamp_date) {
            set_date_input(&ui.start_date, date);
        }
        if let Some(date) = last.and_then(timestamp_date) {
            set_date_input(&ui.end_date, date);
        }
    }
    drop(state_ref);
    refresh_selection_label(ui, state);
    update_suggested_filename(ui, state);
    refresh_navigation(ui, state);
}

fn refresh_selection_label(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    let selected = state
        .borrow()
        .choices
        .iter()
        .filter(|choice| choice.selected.get())
        .count();
    ui.selection_label.set_text(&match selected {
        0 => String::from("0 chats selected"),
        1 => String::from("1 chat selected"),
        count => format!("{count} chats selected"),
    });
}

fn start_export_to(ui: &Ui, state: &Rc<RefCell<AppState>>, output_file: PathBuf) {
    if state.borrow().busy {
        return;
    }
    let Some(index) = state.borrow().index.clone() else {
        return;
    };
    let conversation_ids = state
        .borrow()
        .choices
        .iter()
        .filter(|choice| choice.selected.get())
        .map(|choice| choice.id.clone())
        .collect::<Vec<_>>();
    let Some((start_date, end_date)) = validate_date_range(ui) else {
        return;
    };

    let request = MultiExportRequest {
        conversation_ids,
        start_date: Some(start_date),
        end_date: Some(end_date),
        include_media: ui.include_media.is_active(),
        format: selected_format(ui),
        output_file,
        mode: if ui.separate_mode.is_active() {
            ConversationExportMode::Separate
        } else {
            ConversationExportMode::Combined
        },
    };

    run_export_request(ui, state, index, request, false);
}

fn run_export_request(
    ui: &Ui,
    state: &Rc<RefCell<AppState>>,
    index: Arc<ArchiveIndex>,
    request: MultiExportRequest,
    overwrite_existing: bool,
) {
    if state.borrow().busy {
        return;
    }
    set_busy(ui, state, true, "Exporting chats…");
    let (sender, receiver) = mpsc::channel();
    let worker_index = index.clone();
    let worker_request = request.clone();
    thread::spawn(move || {
        let result = if overwrite_existing {
            export_conversations_overwriting(&worker_index, &worker_request)
        } else {
            export_conversations(&worker_index, &worker_request)
        };
        let _ = sender.send(result);
    });

    let ui = ui.clone();
    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        match receiver.try_recv() {
            Ok(Ok(result)) => {
                set_busy(&ui, &state, false, "Export complete.");
                let details = format!(
                    "Files: {}. Messages: {}. Media: {} copied, {} unavailable.",
                    result.output_files.len(),
                    result.messages_exported,
                    result.media_copied,
                    result.media_missing
                );
                show_export_complete(&ui.window, &details);
                ControlFlow::Break
            }
            Ok(Err(AppError::OutputExists)) if !overwrite_existing => {
                set_busy(&ui, &state, false, "Confirmation required.");
                confirm_overwrite(&ui, &state, index.clone(), request.clone());
                ControlFlow::Break
            }
            Ok(Err(error)) => {
                set_busy(&ui, &state, false, "Export failed.");
                show_app_error(&ui.window, "Could not export chats", &error);
                ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                set_busy(&ui, &state, false, "Export stopped unexpectedly.");
                show_message(
                    &ui.window,
                    MessageType::Error,
                    "Export stopped unexpectedly.",
                    None,
                );
                ControlFlow::Break
            }
        }
    });
}

fn confirm_overwrite(
    ui: &Ui,
    state: &Rc<RefCell<AppState>>,
    index: Arc<ArchiveIndex>,
    request: MultiExportRequest,
) {
    let dialog = MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .message_type(MessageType::Warning)
        .buttons(ButtonsType::None)
        .text("Replace existing export files?")
        .secondary_text("Existing files will be replaced. This cannot be undone.")
        .build();
    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Replace", ResponseType::Accept);
    let ui = ui.clone();
    let state = state.clone();
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response == ResponseType::Accept {
            run_export_request(&ui, &state, index.clone(), request.clone(), true);
        }
    });
    dialog.present();
}

fn selected_format(ui: &Ui) -> ExportFormat {
    if ui.json_format.is_active() {
        ExportFormat::Json
    } else {
        ExportFormat::Markdown
    }
}

fn filename_extension(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Markdown => "md",
        ExportFormat::Json => "json",
    }
}

fn normalized_output_path(path: &Path, format: ExportFormat) -> PathBuf {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(filename_extension(format)))
    {
        path.to_path_buf()
    } else {
        path.with_extension(filename_extension(format))
    }
}

fn filename_slug(value: &str) -> String {
    let mut output = String::new();
    let mut needs_separator = false;
    for character in value.chars() {
        if character.is_alphanumeric() {
            if needs_separator && !output.is_empty() {
                output.push('-');
            }
            for lower in character.to_lowercase() {
                output.push(lower);
            }
            needs_separator = false;
        } else {
            needs_separator = true;
        }
        if output.chars().count() >= 60 {
            break;
        }
    }
    if output.is_empty() {
        String::from("chat")
    } else {
        output.trim_end_matches('-').to_owned()
    }
}

fn format_default_filename(
    selected_count: usize,
    single_name: Option<&str>,
    last_date: NaiveDate,
    separate: bool,
    format: ExportFormat,
) -> String {
    let stem = if separate && selected_count == 1 {
        format!(
            "{}-{}",
            filename_slug(single_name.unwrap_or("chat")),
            last_date.format("%Y-%m-%d")
        )
    } else if separate {
        last_date.format("%Y-%m-%d").to_string()
    } else {
        format!(
            "{selected_count}-conversations-{}",
            last_date.format("%Y-%m-%d")
        )
    };
    format!("{stem}.{}", filename_extension(format))
}

fn update_suggested_filename(ui: &Ui, state: &Rc<RefCell<AppState>>) {
    let suggestion = {
        let state = state.borrow();
        let selected = state
            .choices
            .iter()
            .filter(|choice| choice.selected.get())
            .collect::<Vec<_>>();
        if selected.is_empty() {
            None
        } else {
            let last_date = selected
                .iter()
                .filter_map(|choice| choice.last_timestamp_ms)
                .max()
                .and_then(timestamp_date)
                .unwrap_or_else(|| Local::now().date_naive());
            Some(format_default_filename(
                selected.len(),
                (selected.len() == 1).then_some(selected[0].name.as_str()),
                last_date,
                ui.separate_mode.is_active(),
                selected_format(ui),
            ))
        }
    };

    state.borrow_mut().suggested_filename = suggestion;
}

fn timestamp_date(timestamp_ms: i64) -> Option<NaiveDate> {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|date_time| date_time.date_naive())
}

fn parsed_date(entry: &Entry) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(entry.text().trim(), "%Y-%m-%d").ok()
}

fn validate_date_range(ui: &Ui) -> Option<(NaiveDate, NaiveDate)> {
    let Some(start_date) = parsed_date(&ui.start_date.entry) else {
        show_message(
            &ui.window,
            MessageType::Error,
            "Use YYYY-MM-DD for the start date.",
            None,
        );
        ui.start_date.entry.grab_focus();
        return None;
    };
    let Some(end_date) = parsed_date(&ui.end_date.entry) else {
        show_message(
            &ui.window,
            MessageType::Error,
            "Use YYYY-MM-DD for the end date.",
            None,
        );
        ui.end_date.entry.grab_focus();
        return None;
    };
    if start_date > end_date {
        show_message(
            &ui.window,
            MessageType::Warning,
            "The start date is after the end date.",
            None,
        );
        ui.start_date.entry.grab_focus();
        return None;
    }
    Some((start_date, end_date))
}

fn set_date_input(input: &DateInput, date: NaiveDate) {
    input.entry.set_text(&date.format("%Y-%m-%d").to_string());
    select_calendar_day(&input.calendar, date);
}

fn calendar_date(calendar: &Calendar) -> Option<NaiveDate> {
    let date = calendar.date();
    NaiveDate::from_ymd_opt(
        date.year(),
        u32::try_from(date.month()).ok()?,
        u32::try_from(date.day_of_month()).ok()?,
    )
}

fn select_calendar_day(calendar: &Calendar, date: NaiveDate) {
    if let Ok(date_time) = glib::DateTime::from_local(
        date.year(),
        i32::try_from(date.month()).unwrap_or(1),
        i32::try_from(date.day()).unwrap_or(1),
        12,
        0,
        0.0,
    ) {
        calendar.select_day(&date_time);
    }
}

fn mark_calendar_day(calendar: &Calendar, date: NaiveDate) {
    calendar.clear_marks();
    calendar.mark_day(date.day());
}

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn show_app_error(parent: &ApplicationWindow, title: &str, error: &AppError) {
    show_message(parent, MessageType::Error, title, Some(&error.to_string()));
}

fn show_export_complete(parent: &ApplicationWindow, details: &str) {
    let dialog = MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .message_type(MessageType::Info)
        .buttons(ButtonsType::None)
        .text("Export complete")
        .secondary_text(details)
        .build();
    dialog.add_button("OK", ResponseType::Ok);
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
}

fn show_message(
    parent: &ApplicationWindow,
    message_type: MessageType,
    title: &str,
    details: Option<&str>,
) {
    let dialog = MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .message_type(message_type)
        .buttons(ButtonsType::Close)
        .text(title)
        .secondary_text(details.unwrap_or_default())
        .build();
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widget_tree_has_css_class(widget: &gtk::Widget, class: &str) -> bool {
        if widget.has_css_class(class) {
            return true;
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if widget_tree_has_css_class(&current, class) {
                return true;
            }
            child = current.next_sibling();
        }
        false
    }

    #[test]
    fn default_filename_uses_conversation_name_and_last_message_date() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 20).expect("test date must be valid");
        assert_eq!(
            format_default_filename(
                1,
                Some("Example Conversation"),
                date,
                true,
                ExportFormat::Markdown,
            ),
            "example-conversation-2026-08-20.md"
        );
        assert_eq!(
            format_default_filename(4, None, date, false, ExportFormat::Json),
            "4-conversations-2026-08-20.json"
        );
    }

    #[test]
    fn save_dialog_filename_is_normalized_to_the_selected_format() {
        assert_eq!(
            normalized_output_path(Path::new("conversation"), ExportFormat::Markdown),
            PathBuf::from("conversation.md")
        );
        assert_eq!(
            normalized_output_path(Path::new("conversation.txt"), ExportFormat::Json),
            PathBuf::from("conversation.json")
        );
        assert_eq!(
            normalized_output_path(Path::new("conversation.JSON"), ExportFormat::Json),
            PathBuf::from("conversation.JSON")
        );
    }

    #[test]
    fn timestamp_conversion_rejects_out_of_range_values() {
        assert!(timestamp_date(i64::MAX).is_none());
    }

    #[test]
    fn final_step_and_destination_chooser_match_export_shape() {
        assert_eq!(STEP_NAMES.len(), 4);
        assert_eq!(STEP_TITLES[1], "Select export directory");
        assert_eq!(STEP_TITLES.last(), Some(&"Export options"));
        assert!(!selection_status_is_visible(0));
        assert!(!selection_status_is_visible(1));
        assert!(selection_status_is_visible(2));
        assert!(selection_status_is_visible(3));
        assert!(!export_uses_directory_chooser(false, 2));
        assert!(!export_uses_directory_chooser(true, 1));
        assert!(export_uses_directory_chooser(true, 2));
    }

    #[test]
    fn folder_preferences_round_trip_in_a_private_file() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("config").join("preferences.json");
        let preferences = AppPreferences {
            last_source_folder: Some(PathBuf::from("/synthetic/source")),
            last_destination_folder: Some(PathBuf::from("/synthetic/destination")),
        };

        save_preferences_to(&path, &preferences).expect("preferences should be saved");
        assert_eq!(load_preferences_from(&path), preferences);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path)
                    .expect("preferences metadata should be readable")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().expect("preferences should have a parent"))
                    .expect("configuration directory metadata should be readable")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    #[ignore = "requires a graphical display"]
    fn gtk_ui_uses_native_columns_and_export_dialogs() {
        gtk::init().expect("GTK must initialize");
        assert_conversation_columns_share_native_view_and_default_to_newest_date();
        assert_calendar_day_selection_updates_entry();
        assert_folder_dialog_is_a_native_folder_chooser();
        assert_export_dialogs_use_native_save_or_folder_actions();
    }

    fn assert_calendar_day_selection_updates_entry() {
        let initial = NaiveDate::from_ymd_opt(2026, 8, 20).expect("initial date must be valid");
        let selected = NaiveDate::from_ymd_opt(2026, 9, 7).expect("selected date must be valid");
        let input = build_date_input(initial);
        let _group = date_input_group("Date", &input);

        select_calendar_day(&input.calendar, selected);

        assert_eq!(input.entry.text().as_str(), "2026-09-07");
        assert!(input.calendar.day_is_marked(7));
        assert!(!input.calendar.day_is_marked(20));
    }

    fn assert_conversation_columns_share_native_view_and_default_to_newest_date() {
        install_wizard_styles();
        let (page, _, _, _, store, columns) = build_conversation_page();
        let view = columns[0]
            .column_view()
            .expect("column must belong to a column view");

        assert_eq!(view.columns().n_items(), 3);
        assert!(view.is_single_click_activate());
        assert!(view.shows_column_separators());
        assert_eq!(columns[0].title().as_deref(), Some("Name"));
        assert_eq!(columns[1].title().as_deref(), Some("Messages"));
        assert_eq!(columns[2].title().as_deref(), Some("Date"));
        assert!(
            columns
                .iter()
                .all(|column| column.column_view().as_ref() == Some(&view))
        );

        for (name, timestamp) in [("Older", 100_i64), ("Newest", 200_i64)] {
            store.append(&glib::BoxedAnyObject::new(ConversationChoice {
                id: name.to_owned(),
                name: name.to_owned(),
                message_count: 1,
                selected: Rc::new(Cell::new(false)),
                first_timestamp_ms: Some(timestamp),
                last_timestamp_ms: Some(timestamp),
            }));
        }

        let first = view
            .model()
            .and_then(|model| model.item(0))
            .and_downcast::<glib::BoxedAnyObject>()
            .expect("sorted model must expose a conversation");
        assert_eq!(first.borrow::<ConversationChoice>().name, "Newest");
        let selected = first.borrow::<ConversationChoice>().selected.clone();
        assert!(toggle_visible_conversation(&view, 0));
        assert!(selected.get());
        assert!(toggle_visible_conversation(&view, 0));
        assert!(!selected.get());
        assert!(!toggle_visible_conversation(&view, 99));
        assert!(widget_tree_has_css_class(page.upcast_ref(), "descending"));
        view.sort_by_column(Some(&columns[2]), SortType::Ascending);
        assert!(widget_tree_has_css_class(page.upcast_ref(), "ascending"));
        let first = view
            .model()
            .and_then(|model| model.item(0))
            .and_downcast::<glib::BoxedAnyObject>()
            .expect("ascending model must expose a conversation");
        assert_eq!(first.borrow::<ConversationChoice>().name, "Older");
        let icon_theme = gtk::IconTheme::for_display(
            &gtk::gdk::Display::default().expect("display should be available"),
        );
        assert!(icon_theme.has_icon("pan-up-symbolic"));
        assert!(icon_theme.has_icon("pan-down-symbolic"));
    }

    fn assert_folder_dialog_is_a_native_folder_chooser() {
        install_wizard_styles();
        let window = ApplicationWindow::builder()
            .default_width(1040)
            .default_height(720)
            .build();
        window.present();
        let dialog = build_folder_dialog(&window, "Choose a folder", Path::new("."));
        assert_eq!(dialog.action(), FileChooserAction::SelectFolder);
        assert_eq!(dialog.accept_label().as_deref(), Some("Select"));
        assert_eq!(dialog.cancel_label().as_deref(), Some("Cancel"));
        dialog.show();
        let context = glib::MainContext::default();
        for _ in 0..50 {
            while context.pending() {
                let _ = context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(dialog.is_visible());
        dialog.hide();
        dialog.destroy();
        window.close();
    }

    fn assert_export_dialogs_use_native_save_or_folder_actions() {
        let window = ApplicationWindow::builder().build();
        let save = build_save_dialog(
            &window,
            Path::new("."),
            "conversation-2026-08-20.md",
            ExportFormat::Markdown,
        );
        assert_eq!(save.action(), FileChooserAction::Save);
        assert_eq!(save.accept_label().as_deref(), Some("Export"));
        assert_eq!(
            save.current_name().as_deref(),
            Some("conversation-2026-08-20.md")
        );

        let directory = build_folder_dialog_with_accept(
            &window,
            "Choose destination",
            Path::new("."),
            "Export",
        );
        assert_eq!(directory.action(), FileChooserAction::SelectFolder);
        assert_eq!(directory.accept_label().as_deref(), Some("Export"));
        save.destroy();
        directory.destroy();
    }
}
