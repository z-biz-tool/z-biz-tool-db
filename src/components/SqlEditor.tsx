import React, { useEffect, useRef, useState } from 'react';
import { EditorState, Compartment } from '@codemirror/state';
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter } from '@codemirror/view';
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
import { sql, SQLite, PostgreSQL, MySQL, type SQLNamespace } from '@codemirror/lang-sql';
import { autocompletion, closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete';
import { syntaxHighlighting, defaultHighlightStyle, bracketMatching, foldGutter, foldKeymap } from '@codemirror/language';

interface SqlEditorProps {
  value: string;
  onChange: (value: string) => void;
  height?: string;
  readOnly?: boolean;
  /** ⌘/Ctrl + Enter 的落点；编辑器里按键先走这里，不会多插一个换行 */
  onRun?: () => void;
  /** 编辑器侧的补全延迟。默认 0 = 打到第一个字符就弹 */
  dialect?: 'sqlite' | 'mysql' | 'postgresql';
  /** 已知表与其列，给补全用（只有拿得到结构时才传，缺就退回关键字补全） */
  schema?: SQLNamespace;
}

const dialectOf = (d?: SqlEditorProps['dialect']) =>
  d === 'mysql' ? MySQL : d === 'postgresql' ? PostgreSQL : SQLite;

const SqlEditor: React.FC<SqlEditorProps> = ({
  value,
  onChange,
  height = '200px',
  readOnly = false,
  onRun,
  dialect,
  schema,
}) => {
  const editorRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [isReady, setIsReady] = useState(false);
  // 建好之后不再变的配置放这里；会变的（readOnly/schema）走 reconfigure。
  // onChange / onRun 必须走 ref：这个 effect 只在挂载时跑一次，直接闭包捕获
  // 拿到的是首次渲染的那一份，之后父组件换函数编辑器就再也回调不到新的那一份。
  const latestRef = useRef({ onChange, onRun });
  // readOnly 是唯一能在建好之后改的配置，装进 Compartment 才能 reconfigure
  const roCompartment = useRef(new Compartment());

  useEffect(() => {
    latestRef.current = { onChange, onRun };
  });

  useEffect(() => {
    if (!editorRef.current) return;

    const state = EditorState.create({
      doc: value,
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        highlightActiveLineGutter(),
        history(),
        bracketMatching(),
        foldGutter(),
        closeBrackets(),
        autocompletion(),
        // 只绑语法高亮不绑键位的话，Ctrl-Z / Ctrl-A / Tab 全是浏览器默认行为，
        // history() 装了也按不出来 —— 这套 keymap 才是让它们生效的那一环
        keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...historyKeymap, ...foldKeymap, indentWithTab]),
        keymap.of([
          {
            key: 'Mod-Enter',
            preventDefault: true,
            run: () => {
              latestRef.current.onRun?.();
              return true;
            },
          },
        ]),
        sql({ dialect: dialectOf(dialect), schema, upperCaseKeywords: true }),
        roCompartment.current.of(EditorState.readOnly.of(readOnly)),
        syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) {
            latestRef.current.onChange(update.state.doc.toString());
          }
        }),
      ],
    });

    const view = new EditorView({ state, parent: editorRef.current });
    viewRef.current = view;
    setIsReady(true);

    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // 挂载期配置：改了这些就重建（schema 只在切表时变，重建代价可接受）
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dialect, schema]);

  // 只读切换要立刻生效，不能等重建
  useEffect(() => {
    const view = viewRef.current;
    if (!view || !isReady) return;
    view.dispatch({
      effects: roCompartment.current.reconfigure(EditorState.readOnly.of(readOnly)),
    });
  }, [readOnly, isReady]);

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
