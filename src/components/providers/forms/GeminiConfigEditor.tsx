import React, { useState } from "react";
import { GeminiEnvSection, GeminiConfigSection } from "./GeminiConfigSections";
import { GeminiCommonConfigModal } from "./GeminiCommonConfigModal";

interface GeminiConfigEditorProps {
  envValue: string;
  configValue: string;
  onEnvChange: (value: string) => void;
  onConfigChange: (value: string) => void;
  onEnvBlur?: () => void;
  useCommonConfig: boolean;
  onCommonConfigToggle: (checked: boolean) => void;
  commonConfigSnippet: string;
  onCommonConfigSnippetChange: (value: string) => boolean;
  onCommonConfigErrorClear: () => void;
  commonConfigError: string;
  envError: string;
  configError: string;
  onExtract?: () => void;
  isExtracting?: boolean;
  onEnableAll?: (value: string) => Promise<boolean>;
  isEnablingAll?: boolean;
}

const GeminiConfigEditor: React.FC<GeminiConfigEditorProps> = ({
  envValue,
  configValue,
  onEnvChange,
  onConfigChange,
  onEnvBlur,
  useCommonConfig,
  onCommonConfigToggle,
  commonConfigSnippet,
  onCommonConfigSnippetChange,
  onCommonConfigErrorClear,
  commonConfigError,
  envError,
  configError,
  onExtract,
  isExtracting,
  onEnableAll,
  isEnablingAll,
}) => {
  const [isCommonConfigModalOpen, setIsCommonConfigModalOpen] = useState(false);

  const handleCloseCommonConfigModal = () => {
    onCommonConfigErrorClear();
    setIsCommonConfigModalOpen(false);
  };

  return (
    <div className="space-y-6">
      {/* Env Section */}
      <GeminiEnvSection
        value={envValue}
        onChange={onEnvChange}
        onBlur={onEnvBlur}
        error={envError}
        useCommonConfig={useCommonConfig}
        onCommonConfigToggle={onCommonConfigToggle}
        onEditCommonConfig={() => setIsCommonConfigModalOpen(true)}
        commonConfigError={commonConfigError}
      />

      {/* Config JSON Section */}
      <GeminiConfigSection
        value={configValue}
        onChange={onConfigChange}
        configError={configError}
      />

      {/* Common Config Modal */}
      <GeminiCommonConfigModal
        isOpen={isCommonConfigModalOpen}
        onClose={handleCloseCommonConfigModal}
        value={commonConfigSnippet}
        onSave={onCommonConfigSnippetChange}
        error={commonConfigError}
        onExtract={onExtract}
        isExtracting={isExtracting}
        onEnableAll={onEnableAll}
        isEnablingAll={isEnablingAll}
      />
    </div>
  );
};

export default GeminiConfigEditor;
