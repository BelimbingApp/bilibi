-- Personal layouts now live in account pins.
DELETE FROM base_settings WHERE key IN ('ui.workspace.layouts', 'ui.workspace.default');
