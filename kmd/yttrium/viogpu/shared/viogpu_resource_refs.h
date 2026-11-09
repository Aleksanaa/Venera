// SPDX-License-Identifier: MIT
#pragma once

// One entry per open handle, keyed by host resource id. The caller holds its
// device lock across each update and the corresponding attach/detach command.
class VioGpuResourceRefs
{
  public:
    struct Entry
    {
        Entry *next = nullptr;
        unsigned id = 0;
    };

    bool Acquire(Entry *entry, unsigned id)
    {
        Entry *&head = m_buckets[id % kBuckets];
        bool first = true;
        for (Entry *p = head; p; p = p->next)
            if (p->id == id)
                first = false;
        entry->id = id;
        entry->next = head;
        head = entry;
        return first;
    }

    bool Release(Entry *entry)
    {
        Entry **link = &m_buckets[entry->id % kBuckets];
        bool last = true;
        while (*link)
        {
            if (*link == entry)
            {
                *link = entry->next;
                continue;
            }
            if ((*link)->id == entry->id)
                last = false;
            link = &(*link)->next;
        }
        entry->next = nullptr;
        return last;
    }

  private:
    static constexpr unsigned kBuckets = 64;
    Entry *m_buckets[kBuckets] = {};
};
