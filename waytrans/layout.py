"""
Smart dialogue layout, text clustering, and typography engine for waytrans.
Groups fragmented OCR bounding boxes into coherent dialogue blocks and computes
optimal subtitle positioning, padding, and font metrics.
"""

from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple
import re

# Comprehensive Japanese Kana + Kanji pattern
CJK_REGEX = re.compile(r'[\u3040-\u309f\u30a0-\u30ff\u4e00-\u9faf]')


def is_valid_japanese_dialogue(text: str) -> bool:
    """
    Filters out OCR noise, English web text, code snippets, and UI hallucination symbols.
    Ensures that real Japanese dialogue, menus, choices, and narration trigger translation.
    """
    cleaned = text.strip()
    if not cleaned:
        return False

    # Extract all CJK characters (Hiragana, Katakana, Kanji)
    cjk_chars = CJK_REGEX.findall(cleaned)
    cjk_count = len(cjk_chars)

    # Must contain at least 1 CJK character
    if cjk_count == 0:
        return False

    # Count Latin letters
    latin_count = len(re.findall(r'[a-zA-Z]', cleaned))

    # Reject if dominantly English (e.g. English web paragraph or code with incidental CJK character)
    if latin_count > cjk_count:
        return False

    # Reject common dark-mode Tesseract box artifacts (e.g. "ロ・ooミぶっmnエーエ")
    if 'ロ' in cleaned and cleaned.count('ロ') >= 2:
        return False

    return True


@dataclass
class TextLine:
    text: str
    left: int
    top: int
    width: int
    height: int

    @property
    def right(self) -> int:
        return self.left + self.width

    @property
    def bottom(self) -> int:
        return self.top + self.height


@dataclass
class DialogueBlock:
    """
    Represents a unified dialogue box or speech bubble composed of one or more text lines.
    """
    lines: List[TextLine]
    left: int
    top: int
    right: int
    bottom: int
    combined_text: str
    avg_line_height: int

    @property
    def width(self) -> int:
        return max(1, self.right - self.left)

    @property
    def height(self) -> int:
        return max(1, self.bottom - self.top)

    @property
    def center_x(self) -> int:
        return self.left + (self.width // 2)


def cluster_text_lines(
    raw_boxes: List[Dict[str, any]],
    max_vertical_gap_ratio: float = 1.6,
    max_horizontal_gap: int = 100
) -> List[DialogueBlock]:
    """
    Clusters nearby, vertically or horizontally adjacent OCR text fragments into coherent
    dialogue blocks using proximity-based spatial union.
    """
    if not raw_boxes:
        return []

    lines = [
        TextLine(
            text=b["text"].strip(),
            left=b["left"],
            top=b["top"],
            width=b["width"],
            height=b["height"]
        )
        for b in raw_boxes if b["text"].strip()
    ]

    if not lines:
        return []

    # Sort lines primarily top-to-bottom, secondarily left-to-right
    lines.sort(key=lambda l: (l.top, l.left))

    clusters: List[List[TextLine]] = []

    for line in lines:
        assigned = False
        for cluster in clusters:
            for member in cluster:
                vert_dist = line.top - member.bottom
                vert_overlap = min(line.bottom, member.bottom) - max(line.top, member.top)
                horiz_overlap = min(line.right, member.right) - max(line.left, member.left)
                horiz_dist = max(line.left - member.right, member.left - line.right)

                avg_h = (line.height + member.height) / 2.0
                max_v_gap = avg_h * max_vertical_gap_ratio

                # 1. New line right beneath member
                if 0 <= vert_dist <= max_v_gap and (horiz_overlap > -40 or horiz_dist < max_horizontal_gap):
                    cluster.append(line)
                    assigned = True
                    break

                # 2. On the same horizontal line
                if vert_overlap > (avg_h * 0.4) and horiz_dist <= max_horizontal_gap:
                    cluster.append(line)
                    assigned = True
                    break

            if assigned:
                break

        if not assigned:
            clusters.append([line])

    dialogue_blocks: List[DialogueBlock] = []
    for cluster in clusters:
        min_left = min(l.left for l in cluster)
        min_top = min(l.top for l in cluster)
        max_right = max(l.right for l in cluster)
        max_bottom = max(l.bottom for l in cluster)
        avg_h = int(sum(l.height for l in cluster) / len(cluster))

        combined = "".join(l.text for l in cluster)

        dialogue_blocks.append(
            DialogueBlock(
                lines=cluster,
                left=min_left,
                top=min_top,
                right=max_right,
                bottom=max_bottom,
                combined_text=combined,
                avg_line_height=max(avg_h, 16)
            )
        )

    return dialogue_blocks


def compute_smart_placement(
    block: DialogueBlock,
    translated_text: str,
    viewport_width: int,
    viewport_height: int,
    scan_offset_x: int = 0,
    scan_offset_y: int = 0
) -> Tuple[int, int, int, int]:
    """
    Computes tidy, balanced subtitle placement:
    - Anchored right above the dialogue block.
    - Proportional, readable font size (15-20px).
    - Compact width fitting the sentence cleanly.
    """
    # Readable subtitle font size
    base_size = int(block.avg_line_height * 0.8)
    font_size = max(14, min(base_size, 20))

    # Calculate optimal wrap width based on text length
    estimated_char_w = font_size * 0.55
    text_len = len(translated_text)
    natural_w = int(text_len * estimated_char_w) + 24

    # Keep subtitle pill compact (do not stretch across whole monitor)
    max_w = max(min(natural_w, 640), 220)
    max_w = min(max_w, viewport_width - 60)

    # Center horizontally over dialogue block
    abs_center_x = scan_offset_x + block.center_x
    pos_x = abs_center_x - (max_w // 2)
    pos_x = max(20, min(pos_x, viewport_width - max_w - 20))

    # Vertical positioning
    lines_est = max(1, (natural_w // max_w) + 1)
    sub_h_est = int(lines_est * font_size * 1.3) + 16

    abs_top = scan_offset_y + block.top
    abs_bottom = scan_offset_y + block.bottom

    margin = 8
    target_y = abs_top - sub_h_est - margin
    if target_y < 15:
        target_y = abs_bottom + margin

    pos_y = max(15, min(target_y, viewport_height - sub_h_est - 15))

    return pos_x, pos_y, font_size, max_w
