import React, { useEffect, useRef, useState } from 'react';
import { EditorState } from '@codemirror/state';
import { EditorView, lineNumbers, highlightActiveLine, highlightActiveLineGutter } from '@codemirror/view';
import { history } from '@codemirror/commands';
import { sql } from '@codemirror/lang-sql';
import { syntaxHighlighting, defaultHighlightStyle, bracketMatching, foldGutter } from '@codemirror/language';

interface SqlEditorProps {
  value: string;
  onChange: (value: string) => void;
  height?: string;
  readOnly?: boolean;
}

const SqlEditor: React.FC<SqlEditorProps> = ({
  value,
  onChange,
  height = '200px',
  readOnly = false,
}) => {
  const editorRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [isReady, setIsReady] = useState(false);

  useEffect(() => {
    if (!editorRef.current) return;

    // T-029: CodeMirror 6 SQL 编辑器
    // 支持 SQL 语法高亮、行号、括号匹配、历史操作
    const state = EditorState.create({
      doc: value,
      extensions: [
        // 基础功能
        lineNumbers(),
        highlightActiveLine(),
        highlightActiveLineGutter(),
        history(),
        bracketMatching(),
        foldGutter(),
        // SQL 语法
        sql(),
        syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
        // 只读模式
        readOnly ? EditorState.readOnly.of(true) : [],
        // 变更回调
        EditorView.updateListener.of((update) => {
          if (update.docChanged) {
            onChange(update.state.doc.toString());
          }
        }),
      ],
    });

    const view = new EditorView({
      state,
      parent: editorRef.current,
    });

    viewRef.current = view;
    setIsReady(true);

    return () => {
      view.destroy();
      viewRef.current = null;
    };
  }, []);

  // 同步外部 value 到编辑器
  useEffect(() => {
    if (viewRef.current && isReady) {
      const currentDoc = viewRef.current.state.doc.toString();
      if (currentDoc !== value) {
        viewRef.current.dispatch({
          changes: { from: 0, to: currentDoc.length, insert: value },
        });
      }
    }
  }, [value, isReady]);

  return (
    <div
      ref={editorRef}
      style={{
        height,
        border: '1px solid var(--ant-color-border)',
        borderRadius: 8,
        overflow: 'hidden',
        backgroundColor: 'var(--ant-color-bg-container)',
      }}
    />
  );
};

export default SqlEditor;
