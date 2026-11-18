//! 聊天模板装配：把消息历史（[`Turn`]）渲染成 prompt 文本。纯函数：同输入同输出，无 IO、无时钟、无随机。
//!
//! 本批内置两种定式模板（`<|im_start|>` 标记式与朴素行式）；算法包自带模板的沙箱渲染器（minijinja 形态）归后续批次。
//! 消息内容是**载荷**：模板只拼接，不解读、不落日志。

use madinfer_backend::{Role, Turn};

/// 聊天模板：角色开标记 + 统一闭标记 + 末尾生成引导（提示引擎接续 assistant）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Template {
    system_open: &'static str,
    user_open: &'static str,
    assistant_open: &'static str,
    tool_open: &'static str,
    close: &'static str,
    generation_lead: &'static str,
}

impl Template {
    /// 标记式（`<|im_start|>role\n … <|im_end|>\n`，llama.cpp 生态通行的对话标记格式）。
    pub const fn markup() -> Self {
        Self {
            system_open: "<|im_start|>system\n",
            user_open: "<|im_start|>user\n",
            assistant_open: "<|im_start|>assistant\n",
            tool_open: "<|im_start|>tool\n",
            close: "<|im_end|>\n",
            generation_lead: "<|im_start|>assistant\n",
        }
    }

    /// 朴素行式（`role: content\n`；调试与仿真核金样用）。
    pub const fn plain() -> Self {
        Self {
            system_open: "system: ",
            user_open: "user: ",
            assistant_open: "assistant: ",
            tool_open: "tool: ",
            close: "\n",
            generation_lead: "assistant: ",
        }
    }

    const fn open(&self, role: Role) -> &'static str {
        match role {
            Role::System => self.system_open,
            Role::User => self.user_open,
            Role::Assistant => self.assistant_open,
            Role::Tool => self.tool_open,
        }
    }

    /// 渲染：逐条 `open(role) + content + close`，末尾追加生成引导。
    pub fn render(&self, turn: &Turn) -> String {
        let capacity: usize = turn
            .messages
            .iter()
            .map(|m| m.content.len() + 32)
            .sum::<usize>()
            + self.generation_lead.len();
        let mut out = String::with_capacity(capacity);
        for message in &turn.messages {
            out.push_str(self.open(message.role));
            out.push_str(&message.content);
            out.push_str(self.close);
        }
        out.push_str(self.generation_lead);
        out
    }
}

impl Default for Template {
    fn default() -> Self {
        Self::markup()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use madinfer_backend::Message;

    fn turn() -> Turn {
        Turn {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "你是摘要器".to_owned(),
                },
                Message {
                    role: Role::User,
                    content: "总结这段".to_owned(),
                },
                Message {
                    role: Role::Assistant,
                    content: "好的".to_owned(),
                },
                Message {
                    role: Role::Tool,
                    content: "{\"n\":1}".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn render_is_a_pure_function() {
        let template = Template::default();
        assert_eq!(template.render(&turn()), template.render(&turn()));
    }

    #[test]
    fn markup_renders_the_exact_wire_form() {
        let text = Template::markup().render(&turn());
        assert_eq!(
            text,
            "<|im_start|>system\n你是摘要器<|im_end|>\n\
             <|im_start|>user\n总结这段<|im_end|>\n\
             <|im_start|>assistant\n好的<|im_end|>\n\
             <|im_start|>tool\n{\"n\":1}<|im_end|>\n\
             <|im_start|>assistant\n"
        );
    }

    #[test]
    fn plain_renders_one_line_per_message() {
        let text = Template::plain().render(&turn());
        assert_eq!(
            text,
            "system: 你是摘要器\nuser: 总结这段\nassistant: 好的\ntool: {\"n\":1}\nassistant: "
        );
    }

    #[test]
    fn empty_turn_renders_only_the_generation_lead() {
        assert_eq!(
            Template::markup().render(&Turn::default()),
            "<|im_start|>assistant\n"
        );
    }
}
