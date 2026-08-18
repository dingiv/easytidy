import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { errMsg } from '../lib/errors';
import type { AppMode } from '../types';

export function useAppMode() {
  const [mode, setMode] = useState<AppMode | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<{ mode: string; name?: string }>('get_app_mode')
      .then((result) => {
        if (result.mode === 'centralized') {
          setMode({ Centralized: null });
        } else if (result.mode === 'per_container') {
          setMode({ PerContainer: { name: result.name ?? '' } });
        } else {
          setError(`未知应用模式：${result.mode}`);
        }
      })
      .catch((err) => {
        console.error('Failed to get app mode:', err);
        setError(errMsg(err, 'Failed to determine app mode'));
      });
  }, []);

  return { mode, error } as const;
}
