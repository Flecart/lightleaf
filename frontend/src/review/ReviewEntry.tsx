// Entry point of the optional AI review feature. The editor imports only this file; the dialog is
// loaded on demand and wrapped in an error boundary, so a failure here can never break the editor.
import { Component, ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';
import { Icon, Modal, Spinner } from '../ui';

class Boundary extends Component<{ onClose: () => void; children: ComponentChildren }, { failed: boolean }> {
  state = { failed: false };
  componentDidCatch(err: unknown) {
    console.error('AI review UI failed', err);
    this.setState({ failed: true });
  }
  render() {
    if (this.state.failed)
      return (
        <Modal title="AI review" onClose={this.props.onClose}>
          <div class="alert error">The review dialog failed to load. The editor is not affected; try reloading the page.</div>
        </Modal>
      );
    return this.props.children;
  }
}

export interface ReviewProps {
  projectId: string;
  onClose: () => void;
  onFinished: () => void;
  onOpenFile: (path: string) => void;
}

export function ReviewEntry(props: Omit<ReviewProps, 'onClose'>) {
  const [open, setOpen] = useState(false);
  const [Dialog, setDialog] = useState<((p: ReviewProps) => any) | null>(null);
  const [loadError, setLoadError] = useState(false);

  const show = async () => {
    setOpen(true);
    if (!Dialog) {
      try {
        const m = await import('./ReviewModal');
        setDialog(() => m.ReviewModal);
      } catch (e) {
        console.error(e);
        setLoadError(true);
      }
    }
  };
  const close = () => setOpen(false);

  return (
    <>
      <button class="tb-btn" title="AI review: comments written into your LaTeX source" onClick={show}>
        <Icon name="star" /> <span class="hide-sm">Review</span>
      </button>
      {open && (
        <Boundary onClose={close}>
          {Dialog ? (
            <Dialog {...props} onClose={close} />
          ) : (
            <Modal title="AI review" onClose={close}>
              {loadError ? <div class="alert error">Could not load the review dialog.</div> : <div class="center"><Spinner /></div>}
            </Modal>
          )}
        </Boundary>
      )}
    </>
  );
}
