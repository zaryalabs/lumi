//! Deterministic post-reading learning surfaces.

use std::collections::HashSet;

use dioxus::prelude::*;
use gloo_net::http::Request;
use lumi_core::{
    AcceptTranscriptCommand, AiContextAttachment, AiExecutionMode, AiSourceScope, AiTask,
    AiTaskStatus, AudioAttachment, AudioRetentionPolicy, AudioUpload,
    ChangeLearningItemStatusCommand, CompleteReadingResponse, CompleteReadingScopeCommand,
    CreateAudioUploadCommand, CreateLearningAttachmentCommand, CreateLearningItemCommand,
    CreateLearningSessionCommand, EvaluateOpenAnswerRequest, GenerateLearningItemsRequest,
    LearningAiEvaluation, LearningAnswer, LearningAnswerPresentation, LearningAnswerSpec,
    LearningAttempt, LearningAttemptOutcome, LearningChallengeGroup, LearningHint,
    LearningHintReveal, LearningItem, LearningItemKind, LearningItemPage, LearningItemStatus,
    LearningOffer, LearningOfferAction, LearningOption, LearningReviewRating, LearningSession,
    LearningSessionId, LearningSessionKind, LearningSessionState, LearningSettings,
    LearningSourceId, LearningSourceScheduleSettings, LearningToday, MaterialId,
    MoveReadingPositionCommand, OpenAnswerEvaluationOutcome, ProviderCredentialState,
    PutProviderCredentialRequest, SelfCheckRating, SnoozeLearningSessionCommand,
    SubmitLearningAttemptCommand, TranscribeAudioCommand, TranscriptArtifact, TranscriptStatus,
    UpdateLearningItemCommand, UpdateLearningOfferCommand, UpdateLearningSettingsCommand,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use web_sys::RequestCredentials;

use super::account::{notify_session_expired, API_BASE};

#[derive(Clone, PartialEq)]
enum CompletionOfferState {
    Saving,
    Hidden,
    Ready(LearningOffer),
    Busy(LearningOffer),
    Failed(String),
}

/// Save the semantic boundary and show at most one non-blocking offer.
#[component]
pub(crate) fn CompletionOffer(
    progress: MoveReadingPositionCommand,
    completion: CompleteReadingScopeCommand,
    csrf_token: String,
    on_open_session: EventHandler<LearningSessionId>,
    on_manage_items: EventHandler<(MaterialId, LearningSourceId)>,
) -> Element {
    let mut state = use_signal(|| CompletionOfferState::Saving);
    let csrf = use_signal(|| csrf_token);
    let initial_progress = progress.clone();
    let initial_completion = completion.clone();
    use_effect(move || {
        state.set(CompletionOfferState::Saving);
        let progress = initial_progress.clone();
        let completion = initial_completion.clone();
        spawn(async move {
            match complete_after_progress(&progress, &completion, &csrf.read()).await {
                Ok(response) if response.offer.should_offer => {
                    state.set(CompletionOfferState::Ready(response.offer));
                }
                Ok(_) => state.set(CompletionOfferState::Hidden),
                Err(message) => state.set(CompletionOfferState::Failed(message)),
            }
        });
    });
    let snapshot = state.read().clone();
    let busy_state = matches!(&snapshot, CompletionOfferState::Busy(_));
    match snapshot {
        CompletionOfferState::Saving | CompletionOfferState::Hidden => rsx! {},
        CompletionOfferState::Failed(message) => rsx! {
            div { class: "learning-offer compact", role: "status",
                p { "Чтение сохранено. Предложение для самопроверки сейчас недоступно." }
                button { class: "text-action", r#type: "button", onclick: move |_| {
                    state.set(CompletionOfferState::Saving);
                    let progress = progress.clone();
                    let completion = completion.clone();
                    spawn(async move {
                        match complete_after_progress(&progress, &completion, &csrf.read()).await {
                            Ok(response) if response.offer.should_offer => state.set(CompletionOfferState::Ready(response.offer)),
                            Ok(_) => state.set(CompletionOfferState::Hidden),
                            Err(error) => state.set(CompletionOfferState::Failed(error)),
                        }
                    });
                }, "Повторить" }
                span { class: "sr-only", "{message}" }
            }
        },
        CompletionOfferState::Ready(offer) | CompletionOfferState::Busy(offer) => {
            let busy = busy_state;
            let session_offer = offer.clone();
            let dismiss_offer = offer.clone();
            let later_offer = offer.clone();
            let disable_offer = offer.clone();
            rsx! {
                aside { class: "learning-offer", aria_label: "Обучение после чтения",
                    p { class: "eyebrow", "Глава завершена" }
                    h2 { "Закрепить прочитанное?" }
                    p {
                        if offer.active_item_count > 0 {
                            "Короткая самопроверка: до {offer.active_item_count.min(7)} заданий. Можно остановиться в любой момент."
                        } else {
                            "Готовых вопросов по этой главе пока нет. Создайте свой — без подключения ИИ."
                        }
                    }
                    div { class: "dialog-actions",
                        if offer.active_item_count > 0 {
                            button { class: "primary-action", r#type: "button", disabled: busy, onclick: move |_| {
                                state.set(CompletionOfferState::Busy(session_offer.clone()));
                                let source_id = session_offer.source.id;
                                spawn(async move {
                                    match create_session(source_id, LearningSessionKind::ImmediateRecall, &csrf.read()).await {
                                        Ok(session) => on_open_session.call(session.id),
                                        Err(message) => state.set(CompletionOfferState::Failed(message)),
                                    }
                                });
                            }, "Проверить себя" }
                        } else {
                            button { class: "primary-action", r#type: "button", disabled: busy, onclick: move |_| {
                                on_manage_items.call((offer.source.material_id, offer.source.id));
                            }, "Создать вопрос" }
                        }
                        button { class: "secondary-action", r#type: "button", disabled: busy, onclick: move |_| dismiss_offer_action(later_offer.clone(), LearningOfferAction::RemindLater, csrf, state), "Напомнить позже" }
                        button { class: "secondary-action", r#type: "button", disabled: busy, onclick: move |_| dismiss_offer_action(dismiss_offer.clone(), LearningOfferAction::NotNow, csrf, state), "Не сейчас" }
                        button { class: "text-action", r#type: "button", disabled: busy, onclick: move |_| dismiss_offer_action(disable_offer.clone(), LearningOfferAction::DisableMaterialOffers, csrf, state), "Не предлагать для материала" }
                    }
                }
            }
        }
    }
}

fn dismiss_offer_action(
    offer: LearningOffer,
    action: LearningOfferAction,
    csrf: Signal<String>,
    mut state: Signal<CompletionOfferState>,
) {
    state.set(CompletionOfferState::Busy(offer.clone()));
    spawn(async move {
        match update_offer(&offer, action, &csrf.read()).await {
            Ok(_) => state.set(CompletionOfferState::Hidden),
            Err(message) => state.set(CompletionOfferState::Failed(message)),
        }
    });
}

/// Material-level item list, creation and revision editor.
#[component]
pub(crate) fn MaterialLearningPage(
    material_id: MaterialId,
    source_id: Option<LearningSourceId>,
    csrf_token: String,
    on_open_session: EventHandler<LearningSessionId>,
) -> Element {
    let mut items = use_signal(Vec::<LearningItem>::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut refresh = use_signal(|| 0_u64);
    let mut editing = use_signal(|| None::<LearningItem>);
    let mut kind = use_signal(|| "single".to_owned());
    let mut prompt = use_signal(String::new);
    let mut answer_a = use_signal(String::new);
    let mut answer_b = use_signal(String::new);
    let mut correct = use_signal(|| "a".to_owned());
    let mut explanation = use_signal(String::new);
    let mut hint_one = use_signal(String::new);
    let mut hint_two = use_signal(String::new);
    let mut schedule_settings = use_signal(|| None::<LearningSourceScheduleSettings>);
    let mut generation_task = use_signal(|| None::<AiTask>);
    let csrf = use_signal(|| csrf_token);
    use_effect(move || {
        let _ = refresh();
        spawn(async move {
            match load_items(material_id).await {
                Ok(page) => items.set(page.items),
                Err(message) => error.set(message),
            }
        });
    });
    use_effect(move || {
        if let Some(source_id) = source_id {
            spawn(async move {
                match load_source_settings(source_id).await {
                    Ok(value) => schedule_settings.set(Some(value)),
                    Err(message) => error.set(message),
                }
            });
        }
    });
    let active_count = items
        .read()
        .iter()
        .filter(|item| item.status == LearningItemStatus::Active)
        .count();
    let explain_count = items
        .read()
        .iter()
        .filter(|item| {
            item.status == LearningItemStatus::Active
                && item.kind == LearningItemKind::ExplainBackPrompt
        })
        .count();
    let effective_source_id = source_id.or_else(|| {
        items
            .read()
            .iter()
            .find(|item| item.status == LearningItemStatus::Active)
            .map(|item| item.source_id)
    });
    rsx! {
        main { id: "main-content", class: "library-view learning-manage-view", aria_label: "Обучение по материалу",
            header { class: "library-hero compact",
                div {
                    p { class: "eyebrow", "Learning core" }
                    h1 { "Вопросы по материалу" }
                    p { class: "library-lead", "Вопросы версионируются, а начатые сессии сохраняют исходную формулировку." }
                }
                if let Some(source_id) = effective_source_id {
                    div { class: "dialog-actions",
                        button { class: "primary-action", r#type: "button", disabled: active_count == 0 || busy(), onclick: move |_| {
                            busy.set(true);
                            spawn(async move {
                                match create_session(source_id, LearningSessionKind::ManualPractice, &csrf.read()).await {
                                    Ok(session) => on_open_session.call(session.id),
                                    Err(message) => error.set(message),
                                }
                                busy.set(false);
                            });
                        }, "Начать самопроверку ({active_count})" }
                        button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                            busy.set(true);
                            spawn(async move {
                                match generate_learning_drafts(source_id, &csrf.read()).await {
                                    Ok(task) => {
                                        generation_task.set(Some(task));
                                        error.set(String::new());
                                    }
                                    Err(message) => error.set(message),
                                }
                                busy.set(false);
                            });
                        }, "Создать тест с AI" }
                        if explain_count > 0 {
                            button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                                busy.set(true);
                                spawn(async move {
                                    match create_session(source_id, LearningSessionKind::ExplainBack, &csrf.read()).await {
                                        Ok(session) => on_open_session.call(session.id),
                                        Err(message) => error.set(message),
                                    }
                                    busy.set(false);
                                });
                            }, "Объяснить своими словами" }
                        }
                        if let Some(settings) = schedule_settings.read().as_ref() {
                            {
                                let paused = settings.paused_at.is_some();
                                rsx! {
                                    button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                                        busy.set(true);
                                        spawn(async move {
                                            match set_source_paused(source_id, !paused, &csrf.read()).await {
                                                Ok(value) => schedule_settings.set(Some(value)),
                                                Err(message) => error.set(message),
                                            }
                                            busy.set(false);
                                        });
                                    }, if paused { "Возобновить повторение" } else { "Пауза повторения" } }
                                }
                            }
                        }
                    }
                }
            }
            if !error().is_empty() {
                p { class: "library-alert", role: "alert", "{error}" }
            }
            if let Some(task) = generation_task.read().as_ref() {
                p { class: "capability-note", role: "status",
                    "AI-задача поставлена в общую очередь. Черновики появятся здесь после проверки результата; они не активируются автоматически. Статус: {task_status_label(task.status)}."
                }
            }
            section { class: "library-section learning-items-section", aria_label: "Сохранённые вопросы",
                h2 { "Задания" }
                if items.read().is_empty() {
                    p { class: "capability-note", "Заданий пока нет." }
                } else {
                    ol { class: "learning-item-list",
                        for item in items.read().clone() {
                            {
                                let edit_item = item.clone();
                                let status_item = item.clone();
                                rsx! {
                                    li {
                                        div {
                                            strong { "{item.current_revision.prompt}" }
                                            span { class: "status-pill ready", "{item_status_label(item.status)}" }
                                        }
                                        p { "{item.current_revision.explanation}" }
                                        div { class: "dialog-actions",
                                            button { class: "secondary-action", r#type: "button", onclick: move |_| {
                                                fill_editor(&edit_item, &mut kind, &mut prompt, &mut answer_a, &mut answer_b, &mut correct, &mut explanation, &mut hint_one, &mut hint_two);
                                                editing.set(Some(edit_item.clone()));
                                            }, "Редактировать" }
                                            button { class: "text-action", r#type: "button", onclick: move |_| {
                                                let item = status_item.clone();
                                                let target = if item.status == LearningItemStatus::Active { LearningItemStatus::Archived } else { LearningItemStatus::Active };
                                                spawn(async move {
                                                    match change_status(&item, target, &csrf.read()).await {
                                                        Ok(_) => refresh += 1,
                                                        Err(message) => error.set(message),
                                                    }
                                                });
                                            }, if item.status == LearningItemStatus::Active { "Архивировать" } else { "Активировать" } }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            section { class: "library-section learning-editor", aria_label: "Редактор вопроса",
                h2 { if editing.read().is_some() { "Новая версия задания" } else { "Создать задание" } }
                if effective_source_id.is_none() {
                    p { class: "capability-note", "Откройте этот экран из предложения после завершённой главы, чтобы привязать новый вопрос к точной версии источника." }
                } else {
                    label { "Тип",
                        select { value: "{kind}", onchange: move |event| kind.set(event.value()),
                            option { value: "single", "Один вариант" }
                            option { value: "open", "Открытый ответ с самопроверкой" }
                            option { value: "hinted", "Вопрос с подсказками" }
                            option { value: "true_false", "Верно / неверно" }
                        }
                    }
                    label { "Вопрос",
                        textarea { rows: "3", value: "{prompt}", oninput: move |event| prompt.set(event.value()) }
                    }
                    if kind() == "single" {
                        label { "Вариант A", input { value: "{answer_a}", oninput: move |event| answer_a.set(event.value()) } }
                        label { "Вариант B", input { value: "{answer_b}", oninput: move |event| answer_b.set(event.value()) } }
                        label { "Правильный вариант",
                            select { value: "{correct}", onchange: move |event| correct.set(event.value()),
                                option { value: "a", "A" }
                                option { value: "b", "B" }
                            }
                        }
                    } else if kind() == "open" || kind() == "hinted" {
                        label { "Пример ответа",
                            textarea { rows: "3", value: "{answer_a}", oninput: move |event| answer_a.set(event.value()) }
                        }
                    } else {
                        label { "Правильный ответ",
                            select { value: "{correct}", onchange: move |event| correct.set(event.value()),
                                option { value: "a", "Верно" }
                                option { value: "b", "Неверно" }
                            }
                        }
                    }
                    label { "Пояснение",
                        textarea { rows: "3", value: "{explanation}", oninput: move |event| explanation.set(event.value()) }
                    }
                    if kind() == "hinted" {
                        label { "Подсказка 1",
                            textarea { rows: "2", value: "{hint_one}", oninput: move |event| hint_one.set(event.value()) }
                        }
                        label { "Подсказка 2 (необязательно)",
                            textarea { rows: "2", value: "{hint_two}", oninput: move |event| hint_two.set(event.value()) }
                        }
                    }
                    div { class: "dialog-actions",
                        button { class: "primary-action", r#type: "button", disabled: busy() || prompt().trim().is_empty(), onclick: move |_| {
                            let Some(source_id) = effective_source_id else { return; };
                            let edit = editing.read().clone();
                            let command = item_command(source_id, &kind(), &prompt(), &answer_a(), &answer_b(), &correct(), &explanation(), &hint_one(), &hint_two());
                            busy.set(true);
                            spawn(async move {
                                let result = match (edit, command) {
                                    (_, Err(message)) => Err(message),
                                    (Some(item), Ok(command)) => update_item(&item, command, &csrf.read()).await,
                                    (None, Ok(command)) => create_item(&command, &csrf.read()).await,
                                };
                                match result {
                                    Ok(_) => {
                                        editing.set(None);
                                        prompt.set(String::new());
                                        answer_a.set(String::new());
                                        answer_b.set(String::new());
                                        explanation.set(String::new());
                                        hint_one.set(String::new());
                                        hint_two.set(String::new());
                                        refresh += 1;
                                    }
                                    Err(message) => error.set(message),
                                }
                                busy.set(false);
                            });
                        }, if editing.read().is_some() { "Сохранить новую версию" } else { "Создать" } }
                        if editing.read().is_some() {
                            button { class: "secondary-action", r#type: "button", onclick: move |_| editing.set(None), "Отмена" }
                        }
                    }
                }
            }
        }
    }
}

/// Bounded due and ready projection for deterministic review.
#[component]
pub(crate) fn ChallengesPage(
    csrf_token: String,
    on_open_session: EventHandler<LearningSessionId>,
) -> Element {
    let mut today = use_signal(|| None::<LearningToday>);
    let mut error = use_signal(String::new);
    let busy = use_signal(|| false);
    let refresh = use_signal(|| 0_u64);
    let mut material_filter = use_signal(|| None::<MaterialId>);
    let csrf = use_signal(|| csrf_token);
    use_effect(move || {
        let _ = refresh();
        spawn(async move {
            match load_today().await {
                Ok(value) => today.set(Some(value)),
                Err(message) => error.set(message),
            }
        });
    });
    let snapshot = today.read().clone();
    rsx! {
        main { id: "main-content", class: "library-view challenges-view", aria_label: "Повторение",
            header { class: "library-hero compact",
                div {
                    p { class: "eyebrow", "Повторение с учётом забывания" }
                    h1 { "Повторение" }
                    p { class: "library-lead", "Короткая очередь на сегодня. Подсказки и открытие источника учитываются, но не превращаются в штраф." }
                }
                if let Some(value) = snapshot.as_ref() {
                    div { class: "challenge-estimate", role: "status",
                        strong { "До {value.settings.daily_limit} заданий" }
                        span { "≈ {value.groups.iter().map(|group| usize::from(group.estimated_minutes)).sum::<usize>()} мин" }
                    }
                }
            }
            if !error().is_empty() {
                p { class: "library-alert", role: "alert", "{error}" }
            }
            if let Some(value) = snapshot {
                section { class: "challenge-settings", aria_label: "Настройки повторения",
                    div {
                        strong { "Расписание" }
                        span { if value.settings.scheduling_enabled { "Включено" } else { "Выключено" } }
                    }
                    button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                        let next = UpdateLearningSettingsCommand {
                            expected_revision: value.settings.object_revision,
                            scheduling_enabled: !value.settings.scheduling_enabled,
                            daily_limit: value.settings.daily_limit,
                            manual_only: value.settings.manual_only,
                        };
                        update_settings_action(next, csrf, busy, error, refresh);
                    }, if value.settings.scheduling_enabled { "Выключить" } else { "Включить" } }
                    button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                        let next = UpdateLearningSettingsCommand {
                            expected_revision: value.settings.object_revision,
                            scheduling_enabled: value.settings.scheduling_enabled,
                            daily_limit: value.settings.daily_limit,
                            manual_only: !value.settings.manual_only,
                        };
                        update_settings_action(next, csrf, busy, error, refresh);
                    }, if value.settings.manual_only { "Вернуть «Сегодня»" } else { "Только вручную" } }
                    label { "Лимит",
                        select { value: "{value.settings.daily_limit}", onchange: move |event| {
                            let Ok(limit) = event.value().parse::<u16>() else { return; };
                            let next = UpdateLearningSettingsCommand {
                                expected_revision: value.settings.object_revision,
                                scheduling_enabled: value.settings.scheduling_enabled,
                                daily_limit: limit,
                                manual_only: value.settings.manual_only,
                            };
                            update_settings_action(next, csrf, busy, error, refresh);
                        },
                            option { value: "5", "5" }
                            option { value: "10", "10" }
                            option { value: "20", "20" }
                            option { value: "40", "40" }
                        }
                    }
                    label { "Материал",
                        select { value: material_filter().map_or_else(|| "all".to_owned(), |id| id.to_string()), onchange: move |event| {
                            material_filter.set(
                                (event.value() != "all")
                                    .then(|| Uuid::parse_str(&event.value()).ok())
                                    .flatten()
                            );
                        },
                            option { value: "all", "Все материалы" }
                            for group in unique_material_groups(&value) {
                                option { value: "{group.material_id}", "{group.title}" }
                            }
                        }
                    }
                }
                section { class: "library-section challenge-today", aria_label: "Сегодня",
                    h2 { "Сегодня" }
                    p { class: "challenge-counts", "К повторению: {value.counts.due} · Закрепить: {value.counts.ready} · Черновики: {value.counts.drafts}" }
                    if !value.settings.scheduling_enabled {
                        p { class: "challenge-empty", "Повторения выключены. Задания и история сохранены; ручная практика доступна ниже." }
                    } else if value.settings.manual_only {
                        p { class: "challenge-empty", "Включён режим «только вручную». Lumi не формирует автоматическую очередь." }
                    } else if filtered_groups(&value.groups, material_filter()).is_empty() {
                        p { class: "challenge-empty", if value.counts.due == 0 { "На сегодня всё выполнено." } else { "Для выбранного материала нет заданий на сегодня." } }
                    } else {
                        div { class: "challenge-grid",
                            for group in filtered_groups(&value.groups, material_filter()) {
                                {challenge_group_card(group, true, csrf, busy, error, on_open_session)}
                            }
                        }
                    }
                }
                section { class: "library-section", aria_label: "Закрепить сейчас",
                    h2 { "Закрепить сейчас" }
                    if filtered_groups(&value.ready_groups, material_filter()).is_empty() {
                        p { class: "challenge-empty", "Новых активных заданий нет." }
                    } else {
                        div { class: "challenge-grid",
                            for group in filtered_groups(&value.ready_groups, material_filter()) {
                                {challenge_group_card(group, false, csrf, busy, error, on_open_session)}
                            }
                        }
                    }
                }
                section { class: "library-section challenge-future", aria_label: "Другие режимы",
                    article {
                        h2 { "Объяснить" }
                        p { "Сформулируйте ответ своими словами, затем сравните его с подсказками и источником. Готовые задания и расписание работают без подключения ИИ." }
                    }
                    article {
                        h2 { "Черновики" }
                        p { "{value.counts.drafts} заданий ожидают активации в материалах." }
                    }
                }
            } else if error().is_empty() {
                p { role: "status", aria_live: "polite", "Собираем ограниченную очередь…" }
            }
        }
    }
}

fn challenge_group_card(
    group: LearningChallengeGroup,
    scheduled: bool,
    csrf: Signal<String>,
    mut busy: Signal<bool>,
    mut error: Signal<String>,
    on_open_session: EventHandler<LearningSessionId>,
) -> Element {
    let source_id = group.source_id;
    rsx! {
        article { class: "challenge-card",
            p { class: "eyebrow", if scheduled { "Повторить" } else { "Новое" } }
            h3 { "{group.title}" }
            p { "{group.item_ids.len()} заданий · ≈ {group.estimated_minutes} мин" }
            button { class: "primary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                busy.set(true);
                spawn(async move {
                    let kind = if scheduled { LearningSessionKind::ScheduledReview } else { LearningSessionKind::ManualPractice };
                    match create_session(source_id, kind, &csrf.read()).await {
                        Ok(session) => on_open_session.call(session.id),
                        Err(message) => error.set(message),
                    }
                    busy.set(false);
                });
            }, if scheduled { "Начать повторение" } else { "Закрепить" } }
        }
    }
}

fn filtered_groups(
    groups: &[LearningChallengeGroup],
    material_id: Option<MaterialId>,
) -> Vec<LearningChallengeGroup> {
    groups
        .iter()
        .filter(|group| material_id.is_none_or(|id| group.material_id == id))
        .cloned()
        .collect()
}

fn unique_material_groups(today: &LearningToday) -> Vec<LearningChallengeGroup> {
    let mut groups = today
        .groups
        .iter()
        .chain(&today.ready_groups)
        .cloned()
        .collect::<Vec<_>>();
    groups.sort_by_key(|group| group.material_id);
    groups.dedup_by_key(|group| group.material_id);
    groups
}

fn update_settings_action(
    command: UpdateLearningSettingsCommand,
    csrf: Signal<String>,
    mut busy: Signal<bool>,
    mut error: Signal<String>,
    mut refresh: Signal<u64>,
) {
    busy.set(true);
    spawn(async move {
        match save_learning_settings(&command, &csrf.read()).await {
            Ok(_) => refresh += 1,
            Err(message) => error.set(message),
        }
        busy.set(false);
    });
}

/// Reload-safe deterministic learning session runner.
#[component]
pub(crate) fn LearningSessionPage(
    session_id: LearningSessionId,
    csrf_token: String,
    on_open_source: EventHandler<(MaterialId, LearningSessionId)>,
    on_close: EventHandler<()>,
) -> Element {
    let mut session = use_signal(|| None::<LearningSession>);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut selected = use_signal(HashSet::<String>::new);
    let mut text_answer = use_signal(String::new);
    let mut revealed_item = use_signal(|| None::<LearningItem>);
    let mut self_check = use_signal(|| None::<SelfCheckRating>);
    let mut review_rating = use_signal(|| None::<LearningReviewRating>);
    let mut refresh = use_signal(|| 0_u64);
    let mut ai_evaluations = use_signal(Vec::<LearningAiEvaluation>::new);
    let mut evaluation_task = use_signal(|| None::<AiTask>);
    let mut voice_recording = use_signal(|| false);
    let mut voice_seconds = use_signal(|| 0_u32);
    let mut voice_draft = use_signal(|| None::<RecordedAudio>);
    let mut voice_preview_url = use_signal(String::new);
    let mut voice_transcript = use_signal(|| None::<TranscriptArtifact>);
    let mut voice_status = use_signal(String::new);
    let mut openai_key = use_signal(String::new);
    let csrf = use_signal(|| csrf_token);
    use_effect(move || {
        let _ = refresh();
        spawn(async move {
            match load_session(session_id).await {
                Ok(value) if value.state == LearningSessionState::Offered => {
                    match transition_session(session_id, "start", &csrf.read()).await {
                        Ok(started) => session.set(Some(started)),
                        Err(message) => error.set(message),
                    }
                }
                Ok(value) => session.set(Some(value)),
                Err(message) => error.set(message),
            }
            if let Ok(values) = load_ai_evaluations(session_id).await {
                ai_evaluations.set(values);
            }
        });
    });
    let current = session
        .read()
        .as_ref()
        .and_then(|value| {
            value.items.iter().find(|item| {
                !value
                    .attempts
                    .iter()
                    .any(|attempt| attempt.item_id == item.item_id)
            })
        })
        .cloned();
    let last_attempt = session
        .read()
        .as_ref()
        .and_then(|value| value.attempts.last())
        .cloned();
    let session_snapshot = session.read().clone();
    let voice_snapshot = voice_transcript.read().clone();
    let voice_draft_snapshot = voice_draft.read().clone();
    let voice_preview_snapshot = voice_preview_url.read().clone();
    rsx! {
        main { id: "main-content", class: "learning-session-view", aria_label: "Сессия самопроверки",
            header { class: "learning-session-header",
                button { class: "secondary-action", r#type: "button", onclick: move |_| on_close.call(()), "Сохранить и выйти" }
                div {
                    p { class: "eyebrow", "Самопроверка" }
                    h1 { if let Some(value) = session.read().as_ref() { "{value.source.title}" } else { "Загружаем…" } }
                }
                if let Some(value) = session.read().as_ref() {
                    span { role: "status", "{value.attempts.len()} / {value.items.len()}" }
                    if value.kind == LearningSessionKind::ScheduledReview {
                        button { class: "text-action", r#type: "button", disabled: busy(), onclick: move |_| {
                            busy.set(true);
                            spawn(async move {
                                match snooze(session_id, 1, &csrf.read()).await {
                                    Ok(_) => on_close.call(()),
                                    Err(message) => error.set(message),
                                }
                                busy.set(false);
                            });
                        }, "Отложить на завтра" }
                    }
                }
            }
            if !error().is_empty() {
                p { class: "library-alert", role: "alert", "{error}" }
            }
            if let Some(value) = session_snapshot {
                if value.state == LearningSessionState::Completed {
                    section { class: "learning-complete-card",
                        h2 { "Сессия завершена" }
                        p { "Ответов сохранено: {value.attempts.len()}. Неотвеченные задания не оценивались." }
                        button { class: "primary-action", r#type: "button", onclick: move |_| on_close.call(()), "Готово" }
                    }
                } else if let Some(item) = current.clone() {
                    {
                    let submit_item = item.clone();
                    let source_item = item.clone();
                    let source_session = value.clone();
                    let source_material_id = value.source.material_id;
                    rsx! {
                    section { class: "learning-question-card", aria_label: "Задание {item.position + 1}",
                        p { class: "eyebrow", "Задание {item.position + 1} из {value.items.len()}" }
                        h2 { "{item.prompt}" }
                        if item.revealed_hint_count > 0 {
                            ol { class: "learning-hints", aria_label: "Открытые подсказки",
                                for hint in item.hints.iter().take(usize::from(item.revealed_hint_count)) {
                                    li { "{hint.text}" }
                                }
                            }
                        }
                        if usize::from(item.revealed_hint_count) < item.hints.len() {
                            {
                                let hint_item_id = item.item_id;
                                let next_position = item.revealed_hint_count.saturating_add(1);
                                rsx! {
                                    button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                                        busy.set(true);
                                        spawn(async move {
                                            match reveal_hint(session_id, hint_item_id, next_position, &csrf.read()).await {
                                                Ok(_) => refresh += 1,
                                                Err(message) => error.set(message),
                                            }
                                            busy.set(false);
                                        });
                                    }, "Открыть подсказку {next_position}" }
                                }
                            }
                        }
                        {answer_input(&item.answer, selected, text_answer)}
                        if matches!(item.answer, LearningAnswerPresentation::OpenText | LearningAnswerPresentation::ExplainBack) {
                            section { class: "learning-voice-answer", aria_label: "Голосовой ответ",
                                h3 { "Ответить голосом" }
                                p { class: "capability-note",
                                    "После записи аудио сохранится в Lumi и будет передано OpenAI Whisper для транскрибации. Перед проверкой вы увидите и подтвердите текст. Исходное аудио удалится после подтверждения."
                                }
                                div { class: "dialog-actions",
                                    if !voice_recording() {
                                        button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                                            spawn(async move {
                                                match begin_voice_recording().await {
                                                    Ok(()) => {
                                                        if !voice_preview_url().is_empty() {
                                                            revoke_voice_preview(&voice_preview_url());
                                                        }
                                                        voice_preview_url.set(String::new());
                                                        voice_draft.set(None);
                                                        voice_seconds.set(0);
                                                        voice_recording.set(true);
                                                        voice_status.set("Идёт запись…".to_owned());
                                                        error.set(String::new());
                                                        spawn(async move {
                                                            while voice_recording() {
                                                                sleep_one_second().await;
                                                                if voice_recording() {
                                                                    voice_seconds += 1;
                                                                }
                                                            }
                                                        });
                                                    }
                                                    Err(message) => error.set(message),
                                                }
                                            });
                                        }, "Записать ответ" }
                                    } else {
                                        button { class: "primary-action", r#type: "button", onclick: move |_| {
                                            busy.set(true);
                                            spawn(async move {
                                                match recorded_audio().await {
                                                    Ok(recording) => {
                                                        let preview = voice_preview(&recording);
                                                        voice_draft.set(Some(recording));
                                                        voice_preview_url.set(preview);
                                                        voice_status.set("Запись готова. Прослушайте её перед отправкой.".to_owned());
                                                        error.set(String::new());
                                                    }
                                                    Err(message) => error.set(message),
                                                }
                                                voice_recording.set(false);
                                                busy.set(false);
                                            });
                                        }, "Остановить запись" }
                                        button { class: "text-action", r#type: "button", onclick: move |_| {
                                            cancel_voice_recording();
                                            voice_recording.set(false);
                                            voice_seconds.set(0);
                                            voice_status.set("Запись отменена.".to_owned());
                                        }, "Отменить запись" }
                                    }
                                }
                                if voice_recording() {
                                    p { role: "timer", aria_live: "off", "Записано: {voice_seconds()} с" }
                                }
                                if let Some(recording) = voice_draft_snapshot.clone() {
                                    audio {
                                        controls: true,
                                        src: "{voice_preview_snapshot}",
                                        aria_label: "Предпрослушивание голосового ответа",
                                    }
                                    div { class: "dialog-actions",
                                        button { class: "primary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                                            let item_id = item.item_id;
                                            let recording = recording.clone();
                                            busy.set(true);
                                            spawn(async move {
                                                match upload_voice_recording(
                                                    session_id,
                                                    item_id,
                                                    recording,
                                                    &csrf.read(),
                                                ).await {
                                                    Ok(transcript) => {
                                                        text_answer.set(transcript.text.clone());
                                                        voice_status.set(transcript_status_label(transcript.status).to_owned());
                                                        voice_transcript.set(Some(transcript));
                                                        revoke_voice_preview(&voice_preview_url());
                                                        voice_preview_url.set(String::new());
                                                        voice_draft.set(None);
                                                        error.set(String::new());
                                                    }
                                                    Err(message) => error.set(message),
                                                }
                                                busy.set(false);
                                            });
                                        }, "Отправить на транскрибацию" }
                                        button { class: "text-action", r#type: "button", disabled: busy(), onclick: move |_| {
                                            revoke_voice_preview(&voice_preview_url());
                                            voice_preview_url.set(String::new());
                                            voice_draft.set(None);
                                            voice_status.set("Запись удалена до отправки.".to_owned());
                                        }, "Удалить запись" }
                                    }
                                }
                                if !voice_status().is_empty() {
                                    p { role: "status", aria_live: "polite", "{voice_status}" }
                                }
                                if let Some(transcript) = voice_snapshot.clone() {
                                    if transcript.status == TranscriptStatus::NeedsReview {
                                        button { class: "secondary-action", r#type: "button", disabled: busy() || text_answer().trim().is_empty(), onclick: move |_| {
                                            let attachment_id = transcript.attachment_id;
                                            let reviewed_text = text_answer.read().clone();
                                            busy.set(true);
                                            spawn(async move {
                                                match accept_voice_transcript(
                                                    attachment_id,
                                                    &reviewed_text,
                                                    &csrf.read(),
                                                ).await {
                                                    Ok(accepted) => {
                                                        voice_status.set("Транскрипт подтверждён. Его можно отправить на проверку.".to_owned());
                                                        voice_transcript.set(Some(accepted));
                                                        error.set(String::new());
                                                    }
                                                    Err(message) => error.set(message),
                                                }
                                                busy.set(false);
                                            });
                                        }, "Подтвердить транскрипт" }
                                    }
                                    if transcript.status == TranscriptStatus::Failed {
                                        label { "Ключ OpenAI для Whisper",
                                            input {
                                                r#type: "password",
                                                autocomplete: "off",
                                                value: "{openai_key}",
                                                oninput: move |event| openai_key.set(event.value()),
                                            }
                                        }
                                        button { class: "text-action", r#type: "button", disabled: busy() || openai_key().trim().is_empty(), onclick: move |_| {
                                            let key = openai_key.read().clone();
                                            busy.set(true);
                                            spawn(async move {
                                                match save_transcription_key(&key, &csrf.read()).await {
                                                    Ok(_) => {
                                                        openai_key.set(String::new());
                                                        voice_status.set("Ключ сохранён. Повторите транскрибацию этой записи.".to_owned());
                                                        error.set(String::new());
                                                    }
                                                    Err(message) => error.set(message),
                                                }
                                                busy.set(false);
                                            });
                                        }, "Сохранить ключ" }
                                        button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                                            let attachment_id = transcript.attachment_id;
                                            busy.set(true);
                                            spawn(async move {
                                                match retry_voice_transcription(attachment_id, &csrf.read()).await {
                                                    Ok(retried) => {
                                                        text_answer.set(retried.text.clone());
                                                        voice_status.set(transcript_status_label(retried.status).to_owned());
                                                        voice_transcript.set(Some(retried));
                                                        error.set(String::new());
                                                    }
                                                    Err(message) => error.set(message),
                                                }
                                                busy.set(false);
                                            });
                                        }, "Повторить транскрибацию" }
                                    }
                                }
                            }
                            div { class: "learning-ai-evaluation",
                                button { class: "secondary-action", r#type: "button", disabled: busy() || text_answer().trim().is_empty() || voice_transcript.read().as_ref().is_some_and(|transcript| transcript.status != TranscriptStatus::Accepted), onclick: move |_| {
                                    let answer = text_answer.read().clone();
                                    let item_id = item.item_id;
                                    busy.set(true);
                                    spawn(async move {
                                        match request_ai_evaluation(session_id, item_id, &answer, &csrf.read()).await {
                                            Ok(task) => {
                                                evaluation_task.set(Some(task));
                                                error.set(String::new());
                                            }
                                            Err(message) => error.set(message),
                                        }
                                        busy.set(false);
                                    });
                                }, if value.kind == LearningSessionKind::ExplainBack { "Получить обратную связь AI" } else { "Проверить ответ с AI" } }
                                if let Some(task) = evaluation_task.read().as_ref() {
                                    p { class: "capability-note", role: "status",
                                        "Оценка выполняется общей AI-очередью: {task_status_label(task.status)}. Можно сохранить self-check, не дожидаясь provider."
                                    }
                                    button { class: "text-action", r#type: "button", onclick: move |_| refresh += 1, "Обновить обратную связь" }
                                }
                                if let Some(evaluation) = ai_evaluations.read().iter().rev().find(|evaluation| evaluation.item_id == item.item_id) {
                                    AiEvaluationFeedback { evaluation: evaluation.clone() }
                                }
                            }
                        }
                        if matches!(item.answer, LearningAnswerPresentation::OpenText | LearningAnswerPresentation::Flashcard) {
                            if revealed_item.read().is_none() {
                                button { class: "secondary-action", r#type: "button", disabled: busy() || (matches!(item.answer, LearningAnswerPresentation::OpenText) && text_answer().trim().is_empty()), onclick: move |_| {
                                    busy.set(true);
                                    spawn(async move {
                                        match load_item(item.item_id).await {
                                            Ok(value) => revealed_item.set(Some(value)),
                                            Err(message) => error.set(message),
                                        }
                                        busy.set(false);
                                    });
                                }, "Показать ответ и оценить себя" }
                            } else if let Some(revealed) = revealed_item.read().as_ref() {
                                div { class: "learning-reveal", role: "region", aria_label: "Ответ для самопроверки",
                                    strong { "Пример ответа" }
                                    p { "{revealed_answer(revealed)}" }
                                    fieldset {
                                        legend { "Как получилось вспомнить?" }
                                        label { input { r#type: "radio", name: "self-check", checked: self_check() == Some(SelfCheckRating::Recalled), onchange: move |_| self_check.set(Some(SelfCheckRating::Recalled)) } "Вспомнил" }
                                        label { input { r#type: "radio", name: "self-check", checked: self_check() == Some(SelfCheckRating::Partial), onchange: move |_| self_check.set(Some(SelfCheckRating::Partial)) } "Частично" }
                                        label { input { r#type: "radio", name: "self-check", checked: self_check() == Some(SelfCheckRating::NotRecalled), onchange: move |_| self_check.set(Some(SelfCheckRating::NotRecalled)) } "Не вспомнил" }
                                    }
                                }
                            }
                        }
                        fieldset { class: "learning-rating",
                            legend { "Насколько легко получилось вспомнить?" }
                            label { input { r#type: "radio", name: "review-rating", checked: review_rating() == Some(LearningReviewRating::Again), onchange: move |_| review_rating.set(Some(LearningReviewRating::Again)) } "Не вспомнил" }
                            label { input { r#type: "radio", name: "review-rating", checked: review_rating() == Some(LearningReviewRating::Hard), onchange: move |_| review_rating.set(Some(LearningReviewRating::Hard)) } "С трудом" }
                            label { input { r#type: "radio", name: "review-rating", checked: review_rating() == Some(LearningReviewRating::Good), onchange: move |_| review_rating.set(Some(LearningReviewRating::Good)) } "Нормально" }
                            label { input { r#type: "radio", name: "review-rating", checked: review_rating() == Some(LearningReviewRating::Easy), onchange: move |_| review_rating.set(Some(LearningReviewRating::Easy)) } "Легко" }
                        }
                        div { class: "dialog-actions",
                            button { class: "primary-action", r#type: "button", disabled: busy() || review_rating().is_none() || !answer_ready(&item.answer, &selected.read(), &text_answer(), self_check()), onclick: move |_| {
                                let command = build_attempt(&submit_item.answer, &selected.read(), &text_answer(), self_check(), review_rating());
                                let Some(command) = command else { return; };
                                busy.set(true);
                                spawn(async move {
                                    match submit_attempt(session_id, submit_item.item_id, &command, &csrf.read()).await {
                                        Ok(_) => {
                                            selected.set(HashSet::new());
                                            text_answer.set(String::new());
                                            revealed_item.set(None);
                                            voice_transcript.set(None);
                                            voice_status.set(String::new());
                                            self_check.set(None);
                                            review_rating.set(None);
                                            refresh += 1;
                                        }
                                        Err(message) => error.set(message),
                                    }
                                    busy.set(false);
                                });
                            }, "Ответить" }
                            button { class: "secondary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                                let attachment = source_attachment(&source_session, &source_item);
                                match crate::ai::stage_reader_target(&attachment) {
                                    Ok(()) => {
                                        let csrf_token = csrf.read().clone();
                                        spawn(async move {
                                            let _ = record_source_opened(session_id, source_item.item_id, &csrf_token).await;
                                            on_open_source.call((source_material_id, session_id));
                                        });
                                    }
                                    Err(message) => error.set(message),
                                }
                            }, "Открыть источник" }
                        }
                    }
                    }
                    }
                    if let Some(attempt) = last_attempt.clone() {
                        AttemptFeedback { attempt }
                    }
                } else {
                    if let Some(attempt) = last_attempt {
                        AttemptFeedback { attempt }
                    }
                    section { class: "learning-complete-card",
                        h2 { "Все задания пройдены" }
                        p { "Завершите сессию, чтобы сохранить итог. Неотвеченных заданий нет." }
                        button { class: "primary-action", r#type: "button", disabled: busy(), onclick: move |_| {
                            busy.set(true);
                            spawn(async move {
                                match transition_session(session_id, "complete", &csrf.read()).await {
                                    Ok(value) => session.set(Some(value)),
                                    Err(message) => error.set(message),
                                }
                                busy.set(false);
                            });
                        }, "Завершить" }
                    }
                }
            } else if error().is_empty() {
                p { role: "status", aria_live: "polite", "Восстанавливаем сессию…" }
            }
        }
    }
}

#[component]
fn AiEvaluationFeedback(evaluation: LearningAiEvaluation) -> Element {
    let feedback = evaluation.feedback;
    rsx! {
        aside { class: "learning-feedback neutral", aria_live: "polite",
            h3 {
                {match feedback.outcome {
                    OpenAnswerEvaluationOutcome::Understood => "Смысл понят",
                    OpenAnswerEvaluationOutcome::Partial => "Понимание частичное",
                    OpenAnswerEvaluationOutcome::NeedsReview => "Стоит перечитать",
                    OpenAnswerEvaluationOutcome::NotEvaluated => "Оценка невозможна",
                }}
            }
            if feedback.outcome == OpenAnswerEvaluationOutcome::NotEvaluated {
                p { "Контекста или provider-ответа недостаточно для честной оценки. Это не нулевая оценка — используйте self-check." }
            } else {
                if !feedback.correct.is_empty() {
                    p { strong { "Верно: " } "{feedback.correct.join(\"; \")}" }
                }
                if !feedback.missing.is_empty() {
                    p { strong { "Не хватает: " } "{feedback.missing.join(\"; \")}" }
                }
                if !feedback.distorted.is_empty() {
                    p { strong { "Нужно уточнить: " } "{feedback.distorted.join(\"; \")}" }
                }
                p { small { "Source citations: {feedback.citation_ids.join(\", \")}" } }
            }
            if let Some(next) = feedback.next_prompt {
                p { strong { "Следующий шаг: " } "{next}" }
            }
        }
    }
}

#[component]
fn AttemptFeedback(attempt: LearningAttempt) -> Element {
    let class = match attempt.feedback.outcome {
        LearningAttemptOutcome::Correct => "correct",
        LearningAttemptOutcome::Incorrect => "incorrect",
        LearningAttemptOutcome::SelfChecked | LearningAttemptOutcome::NotGraded => "neutral",
    };
    rsx! {
        aside { class: "learning-feedback {class}", aria_live: "polite",
            h2 { "{attempt.feedback.message}" }
            if !attempt.feedback.correct_answers.is_empty() {
                p { strong { "Ответ: " } "{attempt.feedback.correct_answers.join(\", \")}" }
            }
            p { "{attempt.feedback.explanation}" }
            if attempt.source_opened {
                small { "Источник был открыт до ответа." }
            }
            if !attempt.hints_used.is_empty() {
                small { "Использовано подсказок: {attempt.hints_used.len()}." }
            }
        }
    }
}

fn answer_input(
    answer: &LearningAnswerPresentation,
    mut selected: Signal<HashSet<String>>,
    mut text_answer: Signal<String>,
) -> Element {
    match answer {
        LearningAnswerPresentation::SingleChoice { options } => rsx! {
            fieldset { class: "learning-options",
                legend { class: "sr-only", "Выберите один ответ" }
                for option in options.clone() {
                    label {
                        input { r#type: "radio", name: "learning-answer", checked: selected.read().contains(&option.id), onchange: move |_| selected.set(HashSet::from([option.id.clone()])) }
                        span { "{option.label}" }
                    }
                }
            }
        },
        LearningAnswerPresentation::MultipleChoice { options } => rsx! {
            fieldset { class: "learning-options",
                legend { "Выберите все подходящие ответы" }
                for option in options.clone() {
                    label {
                        input { r#type: "checkbox", checked: selected.read().contains(&option.id), onchange: move |event| {
                            if event.checked() { selected.write().insert(option.id.clone()); } else { selected.write().remove(&option.id); }
                        } }
                        span { "{option.label}" }
                    }
                }
            }
        },
        LearningAnswerPresentation::TrueFalse => rsx! {
            fieldset { class: "learning-options",
                legend { class: "sr-only", "Выберите верно или неверно" }
                label { input { r#type: "radio", name: "learning-answer", checked: selected.read().contains("true"), onchange: move |_| selected.set(HashSet::from(["true".to_owned()])) } "Верно" }
                label { input { r#type: "radio", name: "learning-answer", checked: selected.read().contains("false"), onchange: move |_| selected.set(HashSet::from(["false".to_owned()])) } "Неверно" }
            }
        },
        LearningAnswerPresentation::OpenText | LearningAnswerPresentation::Cloze => rsx! {
            label { class: "learning-text-answer", "Ваш ответ",
                textarea { rows: "6", value: "{text_answer}", oninput: move |event| text_answer.set(event.value()) }
            }
        },
        LearningAnswerPresentation::Flashcard => rsx! {
            p { class: "capability-note", "Сформулируйте ответ про себя, затем откройте обратную сторону." }
        },
        LearningAnswerPresentation::ExplainBack | LearningAnswerPresentation::Reflection => rsx! {
            label { class: "learning-text-answer", "Ваш ответ",
                textarea { rows: "6", value: "{text_answer}", oninput: move |event| text_answer.set(event.value()) }
            }
        },
    }
}

fn answer_ready(
    answer: &LearningAnswerPresentation,
    selected: &HashSet<String>,
    text: &str,
    self_check: Option<SelfCheckRating>,
) -> bool {
    match answer {
        LearningAnswerPresentation::SingleChoice { .. }
        | LearningAnswerPresentation::MultipleChoice { .. }
        | LearningAnswerPresentation::TrueFalse => !selected.is_empty(),
        LearningAnswerPresentation::OpenText => !text.trim().is_empty() && self_check.is_some(),
        LearningAnswerPresentation::Flashcard => self_check.is_some(),
        LearningAnswerPresentation::Cloze
        | LearningAnswerPresentation::ExplainBack
        | LearningAnswerPresentation::Reflection => !text.trim().is_empty(),
    }
}

fn build_attempt(
    answer: &LearningAnswerPresentation,
    selected: &HashSet<String>,
    text: &str,
    self_check: Option<SelfCheckRating>,
    review_rating: Option<LearningReviewRating>,
) -> Option<SubmitLearningAttemptCommand> {
    let answer = match answer {
        LearningAnswerPresentation::SingleChoice { .. } => LearningAnswer::SingleChoice {
            option_id: selected.iter().next()?.clone(),
        },
        LearningAnswerPresentation::MultipleChoice { .. } => LearningAnswer::MultipleChoice {
            option_ids: selected.iter().cloned().collect(),
        },
        LearningAnswerPresentation::TrueFalse => LearningAnswer::TrueFalse {
            value: selected.contains("true"),
        },
        LearningAnswerPresentation::Flashcard => LearningAnswer::Revealed,
        LearningAnswerPresentation::OpenText
        | LearningAnswerPresentation::Cloze
        | LearningAnswerPresentation::ExplainBack
        | LearningAnswerPresentation::Reflection => LearningAnswer::Text {
            text: text.to_owned(),
        },
    };
    Some(SubmitLearningAttemptCommand {
        answer,
        self_check,
        elapsed_ms: 0,
        review_rating,
    })
}

fn source_attachment(
    session: &LearningSession,
    item: &lumi_core::LearningSessionItem,
) -> AiContextAttachment {
    let scope = item.source_anchor.as_ref().map_or_else(
        || match session.source.scope_kind {
            lumi_core::LearningScopeKind::ContentUnit => AiSourceScope::Chapter {
                material_id: session.source.material_id,
                revision_id: session.source.document_revision_id,
                scope_ref: session.source.content_unit_id.clone().unwrap_or_default(),
            },
            _ => AiSourceScope::Material {
                material_id: session.source.material_id,
                revision_id: session.source.document_revision_id,
            },
        },
        |anchor| AiSourceScope::Selection {
            material_id: session.source.material_id,
            revision_id: session.source.document_revision_id,
            anchor: Box::new(anchor.clone()),
        },
    );
    AiContextAttachment::Source {
        kind: "learning_source".to_owned(),
        material_id: session.source.material_id,
        revision_id: session.source.document_revision_id,
        scope,
        display_label: format!("Источник задания {}", item.position + 1),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the small browser editor passes independent controlled field values"
)]
fn item_command(
    source_id: LearningSourceId,
    kind: &str,
    prompt: &str,
    answer_a: &str,
    answer_b: &str,
    correct: &str,
    explanation: &str,
    hint_one: &str,
    hint_two: &str,
) -> Result<CreateLearningItemCommand, String> {
    let (item_kind, answer_spec) = match kind {
        "single" => {
            if answer_a.trim().is_empty() || answer_b.trim().is_empty() {
                return Err("Заполните оба варианта ответа.".to_owned());
            }
            (
                LearningItemKind::QuizSingleChoice,
                LearningAnswerSpec::SingleChoice {
                    options: vec![
                        LearningOption {
                            id: "a".to_owned(),
                            label: answer_a.trim().to_owned(),
                        },
                        LearningOption {
                            id: "b".to_owned(),
                            label: answer_b.trim().to_owned(),
                        },
                    ],
                    correct_option_id: correct.to_owned(),
                },
            )
        }
        "open" => (
            LearningItemKind::OpenQuestion,
            LearningAnswerSpec::OpenSelfCheck {
                sample_answer: answer_a.trim().to_owned(),
            },
        ),
        "hinted" => {
            if answer_a.trim().is_empty() || hint_one.trim().is_empty() {
                return Err("Добавьте пример ответа и первую подсказку.".to_owned());
            }
            (
                LearningItemKind::HintedQuestion,
                LearningAnswerSpec::HintedSelfCheck {
                    sample_answer: answer_a.trim().to_owned(),
                },
            )
        }
        _ => (
            LearningItemKind::QuizTrueFalse,
            LearningAnswerSpec::TrueFalse {
                correct: correct == "a",
            },
        ),
    };
    let hints = [hint_one, hint_two]
        .into_iter()
        .filter(|hint| !hint.trim().is_empty())
        .enumerate()
        .map(|(index, hint)| LearningHint {
            position: u16::try_from(index + 1).unwrap_or(u16::MAX),
            text: hint.trim().to_owned(),
            source_anchor: None,
        })
        .collect();
    Ok(CreateLearningItemCommand {
        source_id,
        kind: item_kind,
        status: LearningItemStatus::Active,
        prompt: prompt.trim().to_owned(),
        answer_spec,
        explanation: explanation.trim().to_owned(),
        hints,
        source_anchor: None,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the small browser editor restores independent controlled field signals"
)]
fn fill_editor(
    item: &LearningItem,
    kind: &mut Signal<String>,
    prompt: &mut Signal<String>,
    answer_a: &mut Signal<String>,
    answer_b: &mut Signal<String>,
    correct: &mut Signal<String>,
    explanation: &mut Signal<String>,
    hint_one: &mut Signal<String>,
    hint_two: &mut Signal<String>,
) {
    prompt.set(item.current_revision.prompt.clone());
    explanation.set(item.current_revision.explanation.clone());
    hint_one.set(
        item.current_revision
            .hints
            .first()
            .map(|hint| hint.text.clone())
            .unwrap_or_default(),
    );
    hint_two.set(
        item.current_revision
            .hints
            .get(1)
            .map(|hint| hint.text.clone())
            .unwrap_or_default(),
    );
    match &item.current_revision.answer_spec {
        LearningAnswerSpec::SingleChoice {
            options,
            correct_option_id,
        } => {
            kind.set("single".to_owned());
            answer_a.set(
                options
                    .first()
                    .map(|value| value.label.clone())
                    .unwrap_or_default(),
            );
            answer_b.set(
                options
                    .get(1)
                    .map(|value| value.label.clone())
                    .unwrap_or_default(),
            );
            correct.set(correct_option_id.clone());
        }
        LearningAnswerSpec::OpenSelfCheck { sample_answer } => {
            kind.set("open".to_owned());
            answer_a.set(sample_answer.clone());
            answer_b.set(String::new());
        }
        LearningAnswerSpec::HintedSelfCheck { sample_answer } => {
            kind.set("hinted".to_owned());
            answer_a.set(sample_answer.clone());
            answer_b.set(String::new());
        }
        LearningAnswerSpec::TrueFalse { correct: value } => {
            kind.set("true_false".to_owned());
            correct.set(if *value { "a" } else { "b" }.to_owned());
        }
        _ => {}
    }
}

fn revealed_answer(item: &LearningItem) -> String {
    match &item.current_revision.answer_spec {
        LearningAnswerSpec::OpenSelfCheck { sample_answer }
        | LearningAnswerSpec::HintedSelfCheck { sample_answer } => sample_answer.clone(),
        LearningAnswerSpec::Flashcard { back } => back.clone(),
        _ => String::new(),
    }
}

fn item_status_label(status: LearningItemStatus) -> &'static str {
    match status {
        LearningItemStatus::Draft => "Черновик",
        LearningItemStatus::Active => "Активно",
        LearningItemStatus::Archived => "Архив",
        LearningItemStatus::Rejected => "Отклонено",
    }
}

async fn complete_after_progress(
    progress: &MoveReadingPositionCommand,
    completion: &CompleteReadingScopeCommand,
    csrf: &str,
) -> Result<CompleteReadingResponse, String> {
    put_json(
        &format!("/materials/{}/progress", progress.material_id),
        progress,
        csrf,
    )
    .await?;
    post_json(
        &format!("/materials/{}/reading-completions", completion.material_id),
        completion,
        csrf,
    )
    .await
}

async fn update_offer(
    offer: &LearningOffer,
    action: LearningOfferAction,
    csrf: &str,
) -> Result<LearningOffer, String> {
    patch_json(
        &format!("/materials/{}/learning-settings", offer.source.material_id),
        &UpdateLearningOfferCommand {
            completion_id: offer.completion.id,
            action,
        },
        csrf,
    )
    .await
}

async fn load_items(material_id: MaterialId) -> Result<LearningItemPage, String> {
    get_json(&format!(
        "/learning/items?material_id={material_id}&limit=100"
    ))
    .await
}

async fn load_today() -> Result<LearningToday, String> {
    get_json("/learning/challenges/today").await
}

async fn save_learning_settings(
    command: &UpdateLearningSettingsCommand,
    csrf: &str,
) -> Result<LearningSettings, String> {
    patch_json("/learning/settings", command, csrf).await
}

async fn load_source_settings(
    source_id: LearningSourceId,
) -> Result<LearningSourceScheduleSettings, String> {
    get_json(&format!("/learning/sources/{source_id}/settings")).await
}

async fn set_source_paused(
    source_id: LearningSourceId,
    paused: bool,
    csrf: &str,
) -> Result<LearningSourceScheduleSettings, String> {
    post_empty(
        &format!(
            "/learning/sources/{source_id}/{}",
            if paused { "pause" } else { "resume" }
        ),
        csrf,
    )
    .await
}

async fn create_item(
    command: &CreateLearningItemCommand,
    csrf: &str,
) -> Result<LearningItem, String> {
    post_json("/learning/items", command, csrf).await
}

async fn update_item(
    item: &LearningItem,
    command: CreateLearningItemCommand,
    csrf: &str,
) -> Result<LearningItem, String> {
    patch_json(
        &format!("/learning/items/{}", item.id),
        &UpdateLearningItemCommand {
            expected_revision: item.object_revision,
            prompt: command.prompt,
            answer_spec: command.answer_spec,
            explanation: command.explanation,
            hints: command.hints,
            source_anchor: command.source_anchor,
        },
        csrf,
    )
    .await
}

async fn change_status(
    item: &LearningItem,
    status: LearningItemStatus,
    csrf: &str,
) -> Result<LearningItem, String> {
    post_json(
        &format!(
            "/learning/items/{}/{}",
            item.id,
            if status == LearningItemStatus::Active {
                "activate"
            } else {
                "archive"
            }
        ),
        &ChangeLearningItemStatusCommand {
            expected_revision: item.object_revision,
        },
        csrf,
    )
    .await
}

async fn create_session(
    source_id: LearningSourceId,
    kind: LearningSessionKind,
    csrf: &str,
) -> Result<LearningSession, String> {
    post_json(
        "/learning/sessions",
        &CreateLearningSessionCommand { source_id, kind },
        csrf,
    )
    .await
}

async fn generate_learning_drafts(
    source_id: LearningSourceId,
    csrf: &str,
) -> Result<AiTask, String> {
    post_json(
        &format!("/learning/sources/{source_id}/generation-tasks"),
        &GenerateLearningItemsRequest {
            item_count: 7,
            execution_mode: AiExecutionMode::ExecuteNow,
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn request_ai_evaluation(
    session_id: LearningSessionId,
    item_id: Uuid,
    answer: &str,
    csrf: &str,
) -> Result<AiTask, String> {
    post_json(
        "/learning/open-answer-evaluations",
        &EvaluateOpenAnswerRequest {
            session_id,
            item_id,
            answer: answer.to_owned(),
            execution_mode: AiExecutionMode::ExecuteNow,
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn load_ai_evaluations(
    session_id: LearningSessionId,
) -> Result<Vec<LearningAiEvaluation>, String> {
    get_json(&format!("/learning/sessions/{session_id}/ai-evaluations")).await
}

fn task_status_label(status: AiTaskStatus) -> &'static str {
    match status {
        AiTaskStatus::Queued => "в очереди",
        AiTaskStatus::Running => "выполняется",
        AiTaskStatus::NeedsInput => "нужна настройка provider",
        AiTaskStatus::Succeeded => "готово",
        AiTaskStatus::Failed => "ошибка",
        AiTaskStatus::Cancelled => "отменено",
    }
}

fn transcript_status_label(status: TranscriptStatus) -> &'static str {
    match status {
        TranscriptStatus::Pending => "Запись ждёт транскрибации.",
        TranscriptStatus::Processing => "Whisper расшифровывает запись…",
        TranscriptStatus::NeedsReview => "Проверьте текст и подтвердите транскрипт.",
        TranscriptStatus::Accepted => "Транскрипт подтверждён.",
        TranscriptStatus::Failed => {
            "Транскрибация не выполнена. Проверьте ключ OpenAI и повторите запись."
        }
        TranscriptStatus::Cancelled => "Транскрибация отменена.",
    }
}

use crate::voice::RecordedAudio;

#[cfg(target_arch = "wasm32")]
async fn begin_voice_recording() -> Result<(), String> {
    crate::voice::begin_recording().await
}

#[cfg(not(target_arch = "wasm32"))]
async fn begin_voice_recording() -> Result<(), String> {
    Err("Запись голоса доступна только в Web-сборке.".to_owned())
}

#[cfg(target_arch = "wasm32")]
async fn recorded_audio() -> Result<RecordedAudio, String> {
    crate::voice::finish_recording().await
}

#[cfg(not(target_arch = "wasm32"))]
async fn recorded_audio() -> Result<RecordedAudio, String> {
    Err("Запись голоса доступна только в Web-сборке.".to_owned())
}

#[cfg(target_arch = "wasm32")]
fn voice_preview(recording: &RecordedAudio) -> String {
    crate::voice::preview_url(recording)
}

#[cfg(not(target_arch = "wasm32"))]
fn voice_preview(_recording: &RecordedAudio) -> String {
    String::new()
}

#[cfg(target_arch = "wasm32")]
fn revoke_voice_preview(url: &str) {
    crate::voice::revoke_preview(url);
}

#[cfg(not(target_arch = "wasm32"))]
fn revoke_voice_preview(_url: &str) {}

#[cfg(target_arch = "wasm32")]
fn cancel_voice_recording() {
    crate::voice::cancel_recording();
}

#[cfg(not(target_arch = "wasm32"))]
fn cancel_voice_recording() {}

#[cfg(target_arch = "wasm32")]
async fn sleep_one_second() {
    crate::voice::sleep_one_second().await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn sleep_one_second() {}

async fn upload_voice_recording(
    session_id: LearningSessionId,
    item_id: Uuid,
    recording: RecordedAudio,
    csrf: &str,
) -> Result<TranscriptArtifact, String> {
    let checksum = hex_sha256(&recording.bytes);
    let upload: AudioUpload = post_json(
        "/blobs/uploads",
        &CreateAudioUploadCommand {
            media_type: recording.media_type.clone(),
            byte_length: recording.bytes.len() as u64,
            checksum_sha256: checksum,
        },
        csrf,
    )
    .await?;
    put_audio_bytes(
        &format!("/blobs/uploads/{}", upload.id),
        &recording.media_type,
        recording.bytes,
        csrf,
    )
    .await?;
    let _: AudioUpload =
        post_empty(&format!("/blobs/uploads/{}/complete", upload.id), csrf).await?;
    let attachment: AudioAttachment = post_json(
        "/learning/attachments",
        &CreateLearningAttachmentCommand {
            upload_id: upload.id,
            session_id,
            item_id,
            retention: AudioRetentionPolicy::DeleteAfterTranscript,
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await?;
    post_json(
        &format!("/learning/attachments/{}/transcribe", attachment.id),
        &TranscribeAudioCommand {
            language: None,
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn accept_voice_transcript(
    attachment_id: Uuid,
    text: &str,
    csrf: &str,
) -> Result<TranscriptArtifact, String> {
    post_json(
        &format!("/learning/attachments/{attachment_id}/transcript/accept"),
        &AcceptTranscriptCommand {
            text: text.trim().to_owned(),
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn retry_voice_transcription(
    attachment_id: Uuid,
    csrf: &str,
) -> Result<TranscriptArtifact, String> {
    post_json(
        &format!("/learning/attachments/{attachment_id}/transcribe"),
        &TranscribeAudioCommand {
            language: None,
            idempotency_key: Uuid::now_v7().to_string(),
        },
        csrf,
    )
    .await
}

async fn save_transcription_key(key: &str, csrf: &str) -> Result<ProviderCredentialState, String> {
    let response = Request::put(&format!("{API_BASE}/providers/openai/credential"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .json(&PutProviderCredentialRequest {
            credential: key.to_owned(),
            validation_model: "whisper-1".to_owned(),
            idempotency_key: Uuid::now_v7().to_string(),
        })
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(|error| error.to_string())?;
    parse_response(response).await
}

async fn put_audio_bytes(
    path: &str,
    media_type: &str,
    bytes: Vec<u8>,
    csrf: &str,
) -> Result<(), String> {
    let response = Request::put(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Content-Type", media_type)
        .body(bytes)
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status() == 401 {
        notify_session_expired();
    }
    if !response.ok() {
        return Err(format!("Lumi API вернул HTTP {}.", response.status()));
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

async fn load_session(session_id: LearningSessionId) -> Result<LearningSession, String> {
    get_json(&format!("/learning/sessions/{session_id}")).await
}

async fn load_item(item_id: Uuid) -> Result<LearningItem, String> {
    get_json(&format!("/learning/items/{item_id}")).await
}

async fn transition_session(
    session_id: LearningSessionId,
    action: &str,
    csrf: &str,
) -> Result<LearningSession, String> {
    post_empty(&format!("/learning/sessions/{session_id}/{action}"), csrf).await
}

async fn record_source_opened(
    session_id: LearningSessionId,
    item_id: Uuid,
    csrf: &str,
) -> Result<LearningSession, String> {
    post_empty(
        &format!("/learning/sessions/{session_id}/items/{item_id}/source-opened"),
        csrf,
    )
    .await
}

async fn reveal_hint(
    session_id: LearningSessionId,
    item_id: Uuid,
    position: u16,
    csrf: &str,
) -> Result<LearningHintReveal, String> {
    post_empty(
        &format!("/learning/sessions/{session_id}/items/{item_id}/hints/{position}/reveal"),
        csrf,
    )
    .await
}

async fn snooze(
    session_id: LearningSessionId,
    days: u16,
    csrf: &str,
) -> Result<LearningSession, String> {
    post_json(
        &format!("/learning/sessions/{session_id}/snooze"),
        &SnoozeLearningSessionCommand { days },
        csrf,
    )
    .await
}

async fn submit_attempt(
    session_id: LearningSessionId,
    item_id: Uuid,
    command: &SubmitLearningAttemptCommand,
    csrf: &str,
) -> Result<LearningAttempt, String> {
    post_json(
        &format!("/learning/sessions/{session_id}/items/{item_id}/attempts"),
        command,
        csrf,
    )
    .await
}

async fn get_json<T: for<'de> serde::Deserialize<'de>>(path: &str) -> Result<T, String> {
    let response = Request::get(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .send()
        .await
        .map_err(|error| format!("Сеть/API недоступны: {error}"))?;
    parse_response(response).await
}

async fn post_json<T: serde::Serialize, R: for<'de> serde::Deserialize<'de>>(
    path: &str,
    payload: &T,
    csrf: &str,
) -> Result<R, String> {
    let request = Request::post(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .json(payload)
        .map_err(|error| error.to_string())?;
    parse_response(request.send().await.map_err(|error| error.to_string())?).await
}

async fn put_json<T: serde::Serialize>(
    path: &str,
    payload: &T,
    csrf: &str,
) -> Result<serde_json::Value, String> {
    let request = Request::put(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .json(payload)
        .map_err(|error| error.to_string())?;
    parse_response(request.send().await.map_err(|error| error.to_string())?).await
}

async fn patch_json<T: serde::Serialize, R: for<'de> serde::Deserialize<'de>>(
    path: &str,
    payload: &T,
    csrf: &str,
) -> Result<R, String> {
    let request = Request::patch(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .json(payload)
        .map_err(|error| error.to_string())?;
    parse_response(request.send().await.map_err(|error| error.to_string())?).await
}

async fn post_empty<R: for<'de> serde::Deserialize<'de>>(
    path: &str,
    csrf: &str,
) -> Result<R, String> {
    let response = Request::post(&format!("{API_BASE}{path}"))
        .credentials(RequestCredentials::Include)
        .header("X-Lumi-CSRF", csrf)
        .header("Idempotency-Key", &Uuid::now_v7().to_string())
        .send()
        .await
        .map_err(|error| error.to_string())?;
    parse_response(response).await
}

async fn parse_response<R: for<'de> serde::Deserialize<'de>>(
    response: gloo_net::http::Response,
) -> Result<R, String> {
    if response.status() == 401 {
        notify_session_expired();
    }
    if !response.ok() {
        return Err(format!("Lumi API вернул HTTP {}.", response.status()));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Некорректный learning API response: {error}"))
}
