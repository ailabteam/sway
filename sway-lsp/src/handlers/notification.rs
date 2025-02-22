//! This module is responsible for implementing handlers for Language Server
//! Protocol. This module specifically handles notification messages sent by the Client.

use crate::{
    core::{document, session::Session},
    error::LanguageServerError,
    server_state::{CompilationContext, ServerState, TaskMessage},
};
use lsp_types::{
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, FileChangeType, Url,
};
use std::sync::{atomic::Ordering, Arc};

pub async fn handle_did_open_text_document(
    state: &ServerState,
    params: DidOpenTextDocumentParams,
) -> Result<(), LanguageServerError> {
    let (uri, session) = state
        .sessions
        .uri_and_session_from_workspace(&params.text_document.uri)
        .await?;
    session.handle_open_file(&uri).await;
    // If the token map is empty, then we need to parse the project.
    // Otherwise, don't recompile the project when a new file in the project is opened
    // as the workspace is already compiled.
    if session.token_map().is_empty() {
        let _ = state
            .cb_tx
            .send(TaskMessage::CompilationContext(CompilationContext {
                session: Some(session.clone()),
                uri: Some(uri.clone()),
                version: None,
            }));
        state.is_compiling.store(true, Ordering::SeqCst);

        state.wait_for_parsing().await;
        state
            .publish_diagnostics(uri, params.text_document.uri, session)
            .await;
    }
    Ok(())
}

fn send_new_compilation_request(
    state: &ServerState,
    session: Arc<Session>,
    uri: &Url,
    version: Option<i32>,
) {
    if state.is_compiling.load(Ordering::SeqCst) {
        // If we are already compiling, then we need to retrigger compilation
        state.retrigger_compilation.store(true, Ordering::SeqCst);
    }

    // Check if the channel is full. If it is, we want to ensure that the compilation
    // thread receives only the most recent value.
    if state.cb_tx.is_full() {
        while let Ok(TaskMessage::CompilationContext(_)) = state.cb_rx.try_recv() {
            // Loop will continue to remove `CompilationContext` messages
            // until the channel has no more of them.
        }
    }

    let _ = state
        .cb_tx
        .send(TaskMessage::CompilationContext(CompilationContext {
            session: Some(session.clone()),
            uri: Some(uri.clone()),
            version,
        }));
}

pub async fn handle_did_change_text_document(
    state: &ServerState,
    params: DidChangeTextDocumentParams,
) -> Result<(), LanguageServerError> {
    document::mark_file_as_dirty(&params.text_document.uri).await?;
    let (uri, session) = state
        .sessions
        .uri_and_session_from_workspace(&params.text_document.uri)
        .await?;
    session
        .write_changes_to_file(&uri, params.content_changes)
        .await?;
    send_new_compilation_request(
        state,
        session.clone(),
        &uri,
        Some(params.text_document.version),
    );
    Ok(())
}

pub(crate) async fn handle_did_save_text_document(
    state: &ServerState,
    params: DidSaveTextDocumentParams,
) -> Result<(), LanguageServerError> {
    document::remove_dirty_flag(&params.text_document.uri).await?;
    let (uri, session) = state
        .sessions
        .uri_and_session_from_workspace(&params.text_document.uri)
        .await?;
    session.sync.resync()?;
    send_new_compilation_request(state, session.clone(), &uri, None);
    state.wait_for_parsing().await;
    state
        .publish_diagnostics(uri, params.text_document.uri, session)
        .await;
    Ok(())
}

pub(crate) async fn handle_did_change_watched_files(
    state: &ServerState,
    params: DidChangeWatchedFilesParams,
) -> Result<(), LanguageServerError> {
    for event in params.changes {
        let (uri, session) = state
            .sessions
            .uri_and_session_from_workspace(&event.uri)
            .await?;
        if let FileChangeType::DELETED = event.typ {
            document::remove_dirty_flag(&event.uri).await?;
            let _ = session.remove_document(&uri);
        }
    }
    Ok(())
}
