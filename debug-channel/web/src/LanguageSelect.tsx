import { Field, Select } from '@fluentui/react-components';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  type LanguagePreference,
  savedPreference,
  setLanguagePreference,
} from './i18n';

export function LanguageSelect() {
  const { t } = useTranslation();
  const [preference, setPreference] = useState(savedPreference);
  useEffect(() => {
    const update = () => {
      if (preference === 'system') void setLanguagePreference('system');
    };
    window.addEventListener('languagechange', update);
    return () => window.removeEventListener('languagechange', update);
  }, [preference]);
  return (
    <Field label={t('language')} className="language-select">
      <Select
        value={preference}
        onChange={(_, data) => {
          const next = data.value as LanguagePreference;
          setPreference(next);
          void setLanguagePreference(next);
        }}
      >
        <option value="system">{t('systemDefault')}</option>
        <option value="en">English</option>
        <option value="zh-CN">简体中文</option>
      </Select>
    </Field>
  );
}
