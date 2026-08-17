#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod dedup;
mod organize;
mod protocols;
mod state;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,core_db=debug,core_index=debug".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol("thumb", |ctx, request, responder| {
            protocols::spawn_thumb(ctx.app_handle().clone(), request, responder);
        })
        .register_asynchronous_uri_scheme_protocol("media", |ctx, request, responder| {
            protocols::spawn_media(ctx.app_handle().clone(), request, responder);
        })
        .setup(|app| {
            state::init(app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::add_root,
            commands::list_roots,
            commands::remove_root,
            commands::start_scan,
            commands::cancel_job,
            commands::list_jobs,
            commands::list_active_jobs,
            commands::query_files,
            commands::fetch_rows,
            commands::get_settings,
            commands::set_settings,
            commands::get_excluded_paths,
            commands::set_excluded_paths,
            commands::start_meta_scan,
            commands::start_thumb_warm,
            commands::clear_thumb_queue,
            commands::get_file_meta,
            commands::open_file,
            commands::reveal_file,
            dedup::start_hash_scan,
            dedup::list_dup_groups,
            dedup::get_dup_group,
            commands::pause_job,
            dedup::list_dup_members_brief,
            dedup::dedup_stats,
            dedup::delete_dup_files,
            organize::get_org_settings,
            organize::set_org_settings,
            organize::preview_org_template,
            organize::list_library_roots,
            organize::set_library_root,
            organize::remove_library_root,
            organize::org_preview,
            organize::cancel_org_preview,
            organize::start_org_hash_scan,
            organize::start_organize,
            organize::list_org_batches,
            organize::undo_org_batch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tidymedia");
}
